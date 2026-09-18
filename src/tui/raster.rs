//! Cached visible list rows, composed into a clipped pixel viewport for Kitty.
use ab_glyph::{point, Font, FontArc, FontVec, Glyph, GlyphId, ScaleFont};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::ZlibEncoder, Compression};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Color,
    text::Line,
    widgets::{Paragraph, Widget},
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    io::{self, Write},
    process::Command,
};

#[derive(Clone)]
struct View {
    area: Rect,
    lines: Vec<Line<'static>>,
    offset: u16,
    highlight: Option<(u16, u16, u16)>,
}
#[derive(Default)]
pub struct Lists {
    cell: Option<(u16, u16)>,
    fonts: Vec<FontArc>,
    rows: HashMap<u64, Vec<u8>>,
    cached_bytes: usize,
    next: [Option<View>; 2],
    shown: [Option<u64>; 2],
    overlays: Vec<Rect>,
    dimmed: bool,
}
impl Lists {
    pub fn new(cell: Option<(u16, u16)>) -> Self {
        let mut result = Self {
            cell,
            ..Self::default()
        };
        if cell.is_some() {
            let family = std::env::var("PARU_TUI_FONT")
                .ok()
                .or_else(|| {
                    let path = dirs::config_dir()?.join("kitty/kitty.conf");
                    std::fs::read_to_string(path)
                        .ok()?
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.trim().split_once(char::is_whitespace)?;
                            (key == "font_family").then(|| value.trim().to_owned())
                        })
                })
                .unwrap_or_else(|| "monospace".into());
            for name in [&family, "sans-serif:lang=zh-cn", "sans-serif:charset=25b8"] {
                if let Some(font) = load_font(name) {
                    result.fonts.push(font);
                }
            }
        }
        result
    }
    pub fn enabled(&self) -> bool {
        self.cell.is_some() && !self.fonts.is_empty()
    }
    pub fn resize(&mut self, cell: Option<(u16, u16)>) {
        self.cell = cell;
        self.rows.clear();
        self.cached_bytes = 0;
        // Force repaint even if the new viewport happens to have identical text.
        self.shown.iter_mut().for_each(|hash| {
            if hash.is_some() {
                *hash = Some(0);
            }
        });
    }
    pub fn begin_frame(&mut self) {
        self.next = [None, None];
        self.overlays.clear();
        self.dimmed = false;
    }
    pub fn dim(&mut self) {
        self.dimmed = true;
    }
    pub fn occlude(&mut self, area: Rect) {
        self.overlays.push(area);
    }
    pub fn place(&mut self, slot: usize, area: Rect, lines: Vec<Line<'static>>, fraction: f32) {
        let Some((_, ch)) = self.cell else { return };
        if !self.enabled() || area.is_empty() {
            return;
        }
        self.next[slot] = Some(View {
            area,
            lines,
            offset: (fraction * ch as f32).round() as u16,
            highlight: None,
        });
    }
    /// Clip the accent to the moving selection, using exactly its pixel coordinates.
    /// Logical selection may already be several rows ahead during key repeat.
    pub fn highlight(&mut self, slot: usize, row: f32, column: u16, width: u16) {
        let Some((_, ch)) = self.cell else { return };
        if let Some(view) = &mut self.next[slot] {
            let top = (row.clamp(0.0, view.area.height.saturating_sub(1) as f32) * ch as f32)
                .round() as u16;
            view.highlight = Some((top, column, width));
        }
    }
    fn row(&mut self, line: &Line<'static>, columns: u16) -> Vec<u8> {
        let (cw, ch) = self.cell.unwrap();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        line.hash(&mut hasher);
        columns.hash(&mut hasher);
        (cw, ch).hash(&mut hasher);
        let key = hasher.finish();
        if let Some(row) = self.rows.get(&key) {
            return row.clone();
        }
        let width = columns as usize * cw as usize;
        let em = cell_em(&self.fonts[0], cw, ch);
        let mut pixels = vec![0; width * ch as usize * 4];
        let mut cells = Buffer::empty(Rect::new(0, 0, columns, 1));
        Paragraph::new(line.clone()).render(cells.area, &mut cells);
        let mut column = 0;
        while column < columns {
            let cell = &cells[(column, 0)];
            let span_width = unicode_width::UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
            let left = column as usize * cw as usize;
            let right = ((column + span_width).min(columns) as usize * cw as usize).min(width);
            if let Color::Rgb(r, g, b) = cell.bg {
                for y in 0..ch as usize {
                    for x in left..right {
                        pixels[(y * width + x) * 4..(y * width + x) * 4 + 4]
                            .copy_from_slice(&[r, g, b, 255]);
                    }
                }
            }
            let (r, g, b) = rgb(cell.fg);
            for c in cell.symbol().chars().filter(|c| !c.is_whitespace()) {
                let font = self
                    .fonts
                    .iter()
                    .find(|f| f.glyph_id(c).0 != 0)
                    .unwrap_or(&self.fonts[0]);
                let id = font.glyph_id(c);
                let glyph = cell_glyph(font, id, em, left as f32, (right - left) as f32, ch);
                if let Some(outline) = font.outline_glyph(glyph) {
                    let bounds = outline.px_bounds();
                    outline.draw(|x, y, coverage| {
                        let x = bounds.min.x as i32 + x as i32;
                        let y = bounds.min.y as i32 + y as i32;
                        if x < left as i32 || x >= right as i32 || y < 0 || y >= ch as i32 {
                            return;
                        }
                        let at = (y as usize * width + x as usize) * 4;
                        let old_a = pixels[at + 3] as f32 / 255.0;
                        let alpha = coverage + old_a * (1.0 - coverage);
                        if alpha == 0.0 {
                            return;
                        }
                        for (k, color) in [r, g, b].into_iter().enumerate() {
                            pixels[at + k] = ((color as f32 * coverage
                                + pixels[at + k] as f32 * old_a * (1.0 - coverage))
                                / alpha)
                                .round() as u8;
                        }
                        pixels[at + 3] = (alpha * 255.0).round() as u8;
                    });
                }
            }
            column += span_width;
        }
        if self.rows.len() >= 512 || self.cached_bytes + pixels.len() > 16 * 1024 * 1024 {
            self.rows.clear();
            self.cached_bytes = 0;
        }
        self.cached_bytes += pixels.len();
        self.rows.insert(key, pixels.clone());
        pixels
    }
    pub fn present(&mut self, out: &mut impl Write) -> io::Result<()> {
        for slot in 0..2 {
            let id = super::selection::image_id() + 2 + slot as u32;
            let Some(view) = self.next[slot].clone() else {
                if self.shown[slot].take().is_some() {
                    write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
                }
                continue;
            };
            let mut state = std::collections::hash_map::DefaultHasher::new();
            view.lines.hash(&mut state);
            view.area.hash(&mut state);
            view.offset.hash(&mut state);
            view.highlight.hash(&mut state);
            super::theme::accent().hash(&mut state);
            self.overlays.hash(&mut state);
            self.dimmed.hash(&mut state);
            self.cell.hash(&mut state);
            let hash = state.finish();
            if self.shown[slot] == Some(hash) {
                continue;
            }
            let (cw, ch) = self.cell.unwrap();
            let width = view.area.width as usize * cw as usize;
            let height = view.area.height as usize * ch as usize;
            let mut pixels = vec![0; width * height * 4];
            for (row, line) in view.lines.iter().enumerate() {
                let data = self.row(line, view.area.width);
                for y in 0..ch as usize {
                    let dest = row as isize * ch as isize + y as isize - view.offset as isize;
                    if dest >= 0 && dest < height as isize {
                        let at = dest as usize * width * 4;
                        pixels[at..at + width * 4]
                            .copy_from_slice(&data[y * width * 4..(y + 1) * width * 4]);
                    }
                }
            }
            if let Some((top, column, columns)) = view.highlight {
                accent_region(&mut pixels, width, height, cw, ch, top, column, columns);
            }
            if self.dimmed {
                let (r, g, b) = rgb(super::theme::BORDER);
                for pixel in pixels.chunks_exact_mut(4) {
                    for (k, to) in [r, g, b].into_iter().enumerate() {
                        pixel[k] = (pixel[k] as f32 * 0.3 + to as f32 * 0.7).round() as u8;
                    }
                }
            }
            for overlay in &self.overlays {
                let clipped = view.area.intersection(*overlay);
                for y in clipped.y..clipped.bottom() {
                    let start = (clipped.x - view.area.x) as usize * cw as usize;
                    let end = start + clipped.width as usize * cw as usize;
                    for py in (y - view.area.y) as usize * ch as usize
                        ..(y - view.area.y + 1) as usize * ch as usize
                    {
                        pixels[(py * width + start) * 4..(py * width + end) * 4].fill(0);
                    }
                }
            }
            upload(out, id, width, height, &pixels)?;
            write!(
                out,
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},p=1,z=-1,C=1,q=2;\x1b\\\x1b8",
                view.area.y + 1,
                view.area.x + 1
            )?;
            self.shown[slot] = Some(hash);
        }
        out.flush()
    }
}
// Match the primary monospace advance to a cell, while keeping both axes equal.
// The same em size is shared with fallback fonts; no glyph is stretched to fill
// its cell and extra leading/side bearings remain ordinary whitespace.
fn cell_em(font: &FontArc, cw: u16, ch: u16) -> f32 {
    let advance = font.as_scaled(1.0).h_advance(font.glyph_id('M')).max(0.01);
    let height = (cw as f32 / advance).min(ch as f32);
    height * font.units_per_em().unwrap_or(font.height_unscaled()) / font.height_unscaled()
}
fn cell_glyph(font: &FontArc, id: GlyphId, em: f32, left: f32, width: f32, ch: u16) -> Glyph {
    let height = (em * font.height_unscaled()
        / font.units_per_em().unwrap_or(font.height_unscaled()))
    .min(ch as f32)
    .min(width / font.as_scaled(1.0).h_advance(id).max(0.01));
    let scaled = font.as_scaled(height);
    let advance = scaled.h_advance(id);
    let x = left
        + if advance > 0.0 {
            (width - advance).max(0.0) / 2.0
        } else {
            0.0
        };
    let baseline = (ch as f32 - height) / 2.0 + scaled.ascent();
    id.with_scale_and_position(height, point(x, baseline))
}
#[allow(clippy::too_many_arguments)]
fn accent_region(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    cw: u16,
    ch: u16,
    top: u16,
    column: u16,
    columns: u16,
) {
    let (r, g, b) = rgb(super::theme::TEXT);
    let (ar, ag, ab) = rgb(super::theme::accent());
    for y in top as usize..(top as usize + ch as usize).min(height) {
        for x in column as usize * cw as usize
            ..((column as usize + columns as usize) * cw as usize).min(width)
        {
            let pixel = &mut pixels[(y * width + x) * 4..][..4];
            // Preserve search matches and semantic colors; only the ordinary name
            // foreground changes. Glyph coverage (alpha) stays untouched.
            if pixel[..3] == [r, g, b] {
                pixel[..3].copy_from_slice(&[ar, ag, ab]);
            }
        }
    }
}
fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        _ => (212, 212, 212),
    }
}
fn load_font(name: &str) -> Option<FontArc> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}\n", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let data = std::fs::read(lines.next()?).ok()?;
    let index = lines.next()?.parse().ok()?;
    FontVec::try_from_vec_and_index(data, index)
        .ok()
        .map(FontArc::new)
}
fn upload(
    out: &mut impl Write,
    id: u32,
    width: usize,
    height: usize,
    pixels: &[u8],
) -> io::Result<()> {
    let mut compressed = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressed.write_all(pixels)?;
    let encoded = STANDARD.encode(compressed.finish()?);
    let mut chunks = encoded.as_bytes().chunks(4096).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(
                out,
                "\x1b_Ga=t,f=32,o=z,s={width},v={height},i={id},q=2,m={more};"
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gq=2,m={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glyphs_keep_font_proportions_in_wide_and_tall_terminal_cells() {
        let font = load_font("monospace").unwrap();
        for (cw, ch) in [(10, 20), (18, 33), (24, 30), (12, 42)] {
            let em = cell_em(&font, cw, ch);
            for c in ['M', 'i', 'g', 'W'] {
                let glyph = cell_glyph(&font, font.glyph_id(c), em, 0.0, cw as f32, ch);
                assert_eq!(glyph.scale.x, glyph.scale.y, "{c} must not be stretched");
                let advance = font.as_scaled(glyph.scale).h_advance(glyph.id);
                assert!(glyph.position.x >= 0.0);
                assert!(glyph.position.x + advance <= cw as f32 + 0.001);
                assert!(glyph.scale.y <= ch as f32);
            }
        }
    }
    #[test]
    fn accent_stays_inside_the_animated_stripe_during_repeated_input() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let mut motion = super::super::motion::Motion::default();
        motion.list("list", 0, 100, 10, now);
        let (r, g, b) = rgb(super::super::theme::TEXT);
        let (ar, ag, ab) = rgb(super::super::theme::accent());
        for step in 1..30 {
            let view = motion.list(
                "list",
                step,
                100,
                10,
                now + Duration::from_millis(step as u64 * 25),
            );
            let top = ((view.cursor - view.scroll).clamp(0.0, 9.0) * 20.0).round() as u16;
            let mut pixels = [r, g, b, 123].repeat(40 * 200);
            accent_region(&mut pixels, 40, 200, 10, 20, top, 1, 2);
            for (i, pixel) in pixels.chunks_exact(4).enumerate() {
                let (x, y) = (i % 40, i / 40);
                let inside =
                    (10..30).contains(&x) && (top as usize..top as usize + 20).contains(&y);
                assert_eq!(pixel[..3], if inside { [ar, ag, ab] } else { [r, g, b] });
                assert_eq!(pixel[3], 123);
            }
        }
    }
    #[test]
    fn list_text_scrolls_in_pixels_and_reuses_cached_rows() {
        let mut lists = Lists::new(Some((10, 20)));
        assert!(
            lists.enabled(),
            "fontconfig and a monospace font are required for raster tests"
        );
        let lines = vec![
            Line::default(),
            Line::from("Package alpha 软件包"),
            Line::from("Package beta"),
        ];
        let mut captures = Vec::new();
        for fraction in [0.0, 0.25, 0.5, 0.75, 0.0] {
            lists.begin_frame();
            lists.place(0, Rect::new(2, 5, 40, 8), lines.clone(), fraction);
            let mut output = Vec::new();
            lists.present(&mut output).unwrap();
            captures.push(String::from_utf8(output).unwrap());
            assert_eq!(lists.rows.len(), 3);
        }
        let mut idle = Vec::new();
        lists.present(&mut idle).unwrap();
        assert!(
            idle.is_empty(),
            "idle frames must not upload the list again"
        );
        lists.begin_frame();
        let mut output = Vec::new();
        lists.present(&mut output).unwrap();
        captures.push(String::from_utf8(output).unwrap());
        if let Some(path) = std::env::var_os("PARU_TUI_TEST_RASTER_FRAMES") {
            std::fs::write(path, serde_json::to_vec(&captures).unwrap()).unwrap();
        }
    }
}
