//! A solid selection rectangle underneath text, positioned in Kitty pixel coordinates.
use base64::{engine::general_purpose::STANDARD, Engine};
use crossterm::terminal::{window_size, WindowSize};
use flate2::{write::ZlibEncoder, Compression};
use ratatui::{layout::Rect, style::Color};
use std::io::{self, Write};

pub(super) fn image_id() -> u32 {
    0x7000_0000 | ((std::process::id() & 0x00ff_ffff) << 4)
}
fn kitty() -> bool {
    (std::env::var("TERM").is_ok_and(|s| s == "xterm-kitty")
        || std::env::var("TERM_PROGRAM").is_ok_and(|s| s == "kitty"))
        && std::env::var_os("TMUX").is_none()
        && std::env::var_os("STY").is_none()
}
pub fn cleanup(out: &mut impl Write) -> io::Result<()> {
    if kitty() {
        for slot in 0..4 {
            write!(out, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", image_id() + slot)?;
        }
    }
    Ok(())
}
fn cell_size(size: WindowSize) -> Option<(u16, u16)> {
    let w = size.width.checked_div(size.columns)?;
    let h = size.height.checked_div(size.rows)?;
    (w > 0 && h > 0).then_some((w, h))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placement {
    x: u16,
    y: u16,
    offset: u16,
    offset_x: u16,
    width: u32,
    height: u16,
}
#[derive(Default)]
pub struct SelectionLayer {
    slot: u32,
    cell: Option<(u16, u16)>,
    next: Option<Placement>,
    shown: Option<Placement>,
    image_size: Option<(u32, u16)>,
    image_color: Option<Color>,
}
impl SelectionLayer {
    pub fn detect() -> Self {
        let mut layer = Self::default();
        layer.resize();
        layer
    }
    pub fn tabs() -> Self {
        Self {
            slot: 1,
            ..Self::detect()
        }
    }
    pub fn resize(&mut self) {
        // Ratatui clears the screen on resize, which also erases Kitty placements.
        self.image_size = None;
        self.cell = kitty()
            .then(|| window_size().ok().and_then(cell_size))
            .flatten();
    }
    pub fn enabled(&self) -> bool {
        self.cell.is_some()
    }
    pub fn cell(&self) -> Option<(u16, u16)> {
        self.cell
    }
    pub fn begin_frame(&mut self) {
        self.next = None;
    }
    pub fn place(&mut self, area: Rect, row: f32) {
        let Some((cw, ch)) = self.cell else { return };
        if area.is_empty() {
            return;
        }
        // Clamp the entire rectangle to the list; never paint headings or borders.
        let pixel_y =
            (row.clamp(0.0, area.height.saturating_sub(1) as f32) * ch as f32).round() as u32;
        self.next = Some(Placement {
            x: area.x,
            y: area.y + (pixel_y / ch as u32) as u16,
            offset: (pixel_y % ch as u32) as u16,
            offset_x: 0,
            width: area.width as u32 * cw as u32,
            height: ch,
        });
    }
    pub fn place_tab(&mut self, area: Rect, column: f32, width: u16) {
        let Some((cw, ch)) = self.cell else { return };
        if area.is_empty() || width == 0 {
            return;
        }
        let pixel_x =
            (column.clamp(0.0, area.width.saturating_sub(width) as f32) * cw as f32).round() as u32;
        self.next = Some(Placement {
            x: area.x + (pixel_x / cw as u32) as u16,
            y: area.y,
            offset_x: (pixel_x % cw as u32) as u16,
            offset: 0,
            width: width.min(area.width) as u32 * cw as u32,
            height: ch,
        });
    }
    pub fn occlude_right(&mut self, overlay: Rect) {
        let (Some((cw, ch)), Some(p)) = (self.cell, self.next) else {
            return;
        };
        let top = p.y as u32 * ch as u32 + p.offset as u32;
        let left = p.x as u32 * cw as u32 + p.offset_x as u32;
        if top < overlay.bottom() as u32 * ch as u32
            && top + p.height as u32 > overlay.y as u32 * ch as u32
            && left + p.width > overlay.x as u32 * cw as u32
        {
            let width = (overlay.x as u32 * cw as u32).saturating_sub(left);
            self.next = (width > 0).then_some(Placement { width, ..p });
        }
    }
    pub fn present(&mut self, out: &mut impl Write) -> io::Result<()> {
        let color = super::theme::selection();
        if self.next == self.shown
            && (self.next.is_none()
                || (self.image_color == Some(color)
                    && self.image_size == self.next.map(|p| (p.width, p.height))))
        {
            return Ok(());
        }
        let id = image_id() + self.slot;
        let Some(p) = self.next else {
            write!(out, "\x1b_Ga=d,d=i,i={id},q=2;\x1b\\")?;
            self.shown = None;
            return out.flush();
        };
        if self.image_size != Some((p.width, p.height)) || self.image_color != Some(color) {
            let Color::Rgb(r, g, b) = color else {
                unreachable!()
            };
            let mut compressed = ZlibEncoder::new(Vec::new(), Compression::fast());
            let Color::Rgb(ar, ag, ab) = super::theme::accent() else {
                unreachable!()
            };
            let cw = self.cell.map(|c| c.0).unwrap_or(1) as f32;
            for y in 0..p.height {
                let mut row = Vec::with_capacity(p.width as usize * 4);
                for x in 0..p.width {
                    let half = p.height as f32 * 0.25;
                    let dy = (y as f32 + 0.5 - p.height as f32 / 2.0).abs();
                    let triangle = self.slot == 0
                        && dy < half
                        && x as f32 >= cw * 0.25
                        && (x as f32) < cw * (0.85 - 0.60 * dy / half);
                    let pixel = if triangle { [ar, ag, ab] } else { [r, g, b] };
                    row.extend_from_slice(&pixel);
                    if self.slot == 1 {
                        let radius = p.height as f32 / 2.0;
                        let dx = (radius - (x as f32 + 0.5).min(p.width as f32 - x as f32 - 0.5))
                            .max(0.0);
                        row.push(if dx * dx + dy * dy <= radius * radius {
                            255
                        } else {
                            0
                        });
                    }
                }
                compressed.write_all(&row)?;
            }
            let encoded = STANDARD.encode(compressed.finish()?);
            // Chunk the payload according to the graphics protocol's 4096-byte limit.
            let mut chunks = encoded.as_bytes().chunks(4096).peekable();
            let mut first = true;
            while let Some(chunk) = chunks.next() {
                let more = u8::from(chunks.peek().is_some());
                if first {
                    write!(
                        out,
                        "\x1b_Ga=t,f={},o=z,s={},v={},i={id},q=2,m={more};",
                        if self.slot == 1 { 32 } else { 24 },
                        p.width,
                        p.height
                    )?;
                    first = false;
                } else {
                    write!(out, "\x1b_Gq=2,m={more};")?;
                }
                out.write_all(chunk)?;
                out.write_all(b"\x1b\\")?;
            }
            self.image_size = Some((p.width, p.height));
            self.image_color = Some(color);
        }
        // Native image dimensions keep its height constant when Y is fractional.
        // Reuse one placement so movement replaces it atomically without a trail.
        // This z-index keeps search matches with explicit backgrounds above it.
        write!(
            out,
            "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},p=1,X={},Y={},z=-1073741825,C=1,q=2;\x1b\\\x1b8",
            p.y + 1,
            p.x + 1,
            p.offset_x,
            p.offset
        )?;
        self.shown = Some(p);
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_input_keeps_text_and_stripe_in_the_same_pixel_frame() {
        use ratatui::{style::Style, text::Line};
        use std::time::{Duration, Instant};
        let cell = std::env::var("PARU_TUI_TEST_CELL")
            .ok()
            .and_then(|v| {
                let (w, h) = v.split_once(',')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((10, 20));
        let mut layer = SelectionLayer {
            cell: Some(cell),
            ..Default::default()
        };
        let mut lists = super::super::raster::Lists::new(Some(cell));
        assert!(lists.enabled());
        let mut motion = super::super::motion::Motion::default();
        let now = Instant::now();
        let area = Rect::new(2, 5, 40, 8);
        let mut captures = Vec::new();
        for (index, target) in [0, 4, 9, 12, 7].into_iter().enumerate() {
            let view = motion.list(
                "list",
                target,
                100,
                8,
                now + Duration::from_millis(index as u64 * 35),
            );
            let start = view.scroll.floor() as usize;
            let lines = (start..start + 9)
                .map(|i| {
                    Line::from(format!("  Package-{i:03}"))
                        .style(Style::default().fg(super::super::theme::TEXT))
                })
                .collect();
            layer.begin_frame();
            lists.begin_frame();
            layer.place(area, view.cursor - view.scroll);
            lists.place(0, area, lines, view.scroll.fract());
            lists.highlight(0, view.cursor - view.scroll, 2, 20);
            let mut output = Vec::new();
            layer.present(&mut output).unwrap();
            lists.present(&mut output).unwrap();
            captures.push(String::from_utf8(output).unwrap());
        }
        layer.begin_frame();
        lists.begin_frame();
        let mut output = Vec::new();
        layer.present(&mut output).unwrap();
        lists.present(&mut output).unwrap();
        captures.push(String::from_utf8(output).unwrap());
        if let Some(path) = std::env::var_os("PARU_TUI_TEST_SYNC_FRAMES") {
            std::fs::write(path, serde_json::to_vec(&captures).unwrap()).unwrap();
        }
    }
    #[test]
    fn rectangle_moves_in_pixels_without_reupload_or_color_changes() {
        let mut layer = SelectionLayer {
            cell: Some((10, 20)),
            ..Default::default()
        };
        let area = Rect::new(2, 5, 40, 12);
        layer.place(area, 0.0);
        let mut output = Vec::new();
        layer.present(&mut output).unwrap();
        let initial = String::from_utf8(output).unwrap();
        let mut captures = vec![initial.clone()];
        assert!(initial.contains("a=t,f=24,o=z,s=400,v=20"));
        for row in [0.25, 0.5, 0.75, 1.0] {
            output = Vec::new();
            layer.begin_frame();
            layer.place(area, row);
            layer.present(&mut output).unwrap();
            let frame = String::from_utf8(output).unwrap();
            captures.push(frame.clone());
            assert!(!frame.contains("a=t"));
            assert!(frame.contains(&format!("Y={},z=", (row * 20.0) as u16 % 20)));
            assert!(frame.contains("p=1,"));
        }
        assert_eq!(layer.shown.unwrap().y, 6);
        output = Vec::new();
        layer.begin_frame();
        layer.present(&mut output).unwrap();
        let deleted = String::from_utf8(output).unwrap();
        assert!(deleted.contains("a=d,d=i"));
        captures.push(deleted);
        if let Some(path) = std::env::var_os("PARU_TUI_TEST_SELECTION_FRAMES") {
            std::fs::write(path, serde_json::to_vec(&captures).unwrap()).unwrap();
        }
    }
    #[test]
    fn missing_pixel_size_falls_back_and_rectangle_stays_inside_list() {
        assert!(cell_size(WindowSize {
            rows: 36,
            columns: 140,
            width: 0,
            height: 0
        })
        .is_none());
        let mut layer = SelectionLayer {
            cell: Some((10, 20)),
            ..Default::default()
        };
        let area = Rect::new(2, 5, 40, 12);
        layer.place(area, -10.0);
        assert_eq!(layer.next.unwrap().y, 5);
        layer.place(area, 1000.0);
        assert_eq!(layer.next.unwrap().y, 16);
        assert_eq!(layer.next.unwrap().offset, 0);
        layer.place(area, 0.5);
        layer.occlude_right(Rect::new(22, 5, 60, 6));
        assert_eq!(layer.next.unwrap().width, 200);
    }
}
