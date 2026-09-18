use super::{
    catalog::{AurComment, CommentSpan},
    settings::Language,
    theme::{accent, BLUE, BORDER, MUTED, SURFACE, TEXT, YELLOW},
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkHit {
    pub row: usize,
    pub start: u16,
    pub end: u16,
    pub url: String,
}

pub struct Layout {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkHit>,
}

impl Layout {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn append(&mut self, comments: &[AurComment], width: u16, language: Language) {
        let zh = language == Language::ZhCn;
        for (index, comment) in comments.iter().enumerate() {
            if !self.lines.is_empty() {
                self.lines.push(Line::default());
            }
            self.lines.push(Line::from(Span::styled(
                "─".repeat(width as usize),
                Style::default().fg(BORDER),
            )));
            let (author, date) = comment
                .header
                .split_once(" commented on ")
                .unwrap_or((&comment.header, ""));
            let mut heading = vec![
                Span::styled(format!("{:02}  ", index + 1), Style::default().fg(MUTED)),
                Span::styled(
                    author.to_owned(),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
            ];
            if comment.pinned {
                heading.push(Span::styled(
                    if zh { "  [置顶]" } else { "  [PINNED]" },
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                ));
            }
            self.lines.push(Line::from(heading));
            if !date.is_empty() {
                self.wrap_spans(
                    &[CommentSpan {
                        text: date.to_owned(),
                        url: None,
                    }],
                    width,
                    Style::default().fg(MUTED),
                );
            }
            self.lines.push(Line::default());
            let mut code_open = false;
            for line in &comment.lines {
                let plain = line.text();
                if line.code {
                    if !code_open {
                        self.code_padding(width);
                        code_open = true;
                    }
                    self.code_line(&plain, width);
                } else {
                    if code_open {
                        self.code_padding(width);
                        code_open = false;
                    }
                    if plain.is_empty() {
                        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
                            self.lines.push(Line::default());
                        }
                    } else {
                        self.wrap_spans(&line.spans, width, Style::default().fg(TEXT));
                    }
                }
            }
            if code_open {
                self.code_padding(width);
            }
            while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
                self.lines.pop();
            }
        }
    }

    fn code_padding(&mut self, width: u16) {
        self.lines.push(Line::from(Span::styled(
            " ".repeat(width as usize),
            Style::default().fg(TEXT).bg(SURFACE),
        )));
    }

    fn code_line(&mut self, text: &str, width: u16) {
        let available = width.saturating_sub(4).max(1) as usize;
        let mut row = String::new();
        let mut used = 0;
        let mut chunks = Vec::new();
        for character in text.chars().filter(|character| !character.is_control()) {
            let size = character.width().unwrap_or(0);
            if used + size > available && !row.is_empty() {
                chunks.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(character);
            used += size;
        }
        chunks.push(row);
        for chunk in chunks {
            let pad = available.saturating_sub(chunk.width());
            self.lines.push(Line::from(Span::styled(
                format!("  {chunk}{}  ", " ".repeat(pad)),
                Style::default().fg(TEXT).bg(SURFACE),
            )));
        }
    }

    fn wrap_spans(&mut self, spans: &[CommentSpan], width: u16, base: Style) {
        let available = width.saturating_sub(2).max(1) as usize;
        let mut runs: Vec<(String, Option<String>)> = Vec::new();
        let mut used = 0usize;
        for span in spans {
            for character in span.text.chars() {
                if character == '\n' {
                    if !runs.is_empty() {
                        self.finish_row(&runs, base);
                        runs.clear();
                        used = 0;
                    }
                    continue;
                }
                if character.is_control() {
                    continue;
                }
                let size = character.width().unwrap_or(0);
                if used + size > available && used > 0 {
                    self.finish_row(&runs, base);
                    runs.clear();
                    used = 0;
                }
                if character == ' ' && used == 0 {
                    continue;
                }
                if let Some(last) = runs.last_mut().filter(|run| run.1 == span.url) {
                    last.0.push(character);
                } else {
                    runs.push((character.to_string(), span.url.clone()));
                }
                used += size;
            }
        }
        if !runs.is_empty() {
            self.finish_row(&runs, base);
        }
    }

    fn finish_row(&mut self, runs: &[(String, Option<String>)], base: Style) {
        let row = self.lines.len();
        let mut col = 2u16;
        let mut spans = vec![Span::raw("  ")];
        for (text, url) in runs {
            let size = text.width().min(u16::MAX as usize) as u16;
            let style = if url.is_some() {
                Style::default().fg(BLUE).add_modifier(Modifier::UNDERLINED)
            } else {
                base
            };
            spans.push(Span::styled(text.clone(), style));
            if let Some(url) = url {
                self.links.push(LinkHit {
                    row,
                    start: col,
                    end: col.saturating_add(size),
                    url: url.clone(),
                });
            }
            col = col.saturating_add(size);
        }
        self.lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::catalog::{CommentLine, CommentSpan};

    #[test]
    fn code_blocks_fill_the_panel_and_links_track_wrapped_rows() {
        let comments = vec![AurComment {
            id: "1".into(),
            header: "alice commented on 2026-09-16".into(),
            pinned: true,
            lines: vec![
                CommentLine {
                    spans: vec![
                        CommentSpan {
                            text: "Read ".into(),
                            url: None,
                        },
                        CommentSpan {
                            text: "https://example.org/long/path".into(),
                            url: Some("https://example.org/long/path".into()),
                        },
                    ],
                    code: false,
                },
                CommentLine::plain("$ makepkg -si", true),
            ],
        }];
        let mut layout = Layout::new();
        layout.append(&comments, 18, Language::En);
        assert!(layout.links.len() >= 2);
        assert!(layout.links.iter().all(|hit| hit.end <= 18));
        let code = layout.lines.iter().filter(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.style.bg == Some(SURFACE))
        });
        assert!(code.count() >= 3);
        assert!(layout
            .lines
            .iter()
            .filter(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.style.bg == Some(SURFACE))
            })
            .all(|line| line.width() == 18));
    }
}
