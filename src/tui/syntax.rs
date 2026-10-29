use super::theme::{BLUE, GREEN, MUTED as DIM, TEXT, YELLOW, accent};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
    Backtick,
}

pub fn pkgbuild(source: &str) -> Vec<Line<'static>> {
    let mut quote = Quote::None;
    let mut lines: Vec<_> = source
        .lines()
        .map(|line| highlight_line(&display_line(line), &mut quote))
        .collect();
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

// A terminal interprets a literal tab as cursor movement. Expand it before
// layout so Ratatui's cell positions and the terminal's cursor agree, including
// when a tabbed line enters or leaves the viewport during incremental redraws.
fn display_line(line: &str) -> String {
    let mut display = String::new();
    let mut column = 0;
    for (index, part) in line.split('\t').enumerate() {
        if index > 0 {
            let padding = 8 - column % 8;
            display.extend(std::iter::repeat_n(' ', padding));
            column += padding;
        }
        let part: String = part.chars().filter(|c| !c.is_control()).collect();
        column += part.width();
        display.push_str(&part);
    }
    display
}

fn highlight_line(line: &str, quote: &mut Quote) -> Line<'static> {
    let mut spans = Vec::new();
    let mut at = 0;
    while at < line.len() {
        match *quote {
            Quote::Single => quoted(line, &mut at, quote, '\'', &mut spans, false),
            Quote::Double => quoted(line, &mut at, quote, '"', &mut spans, true),
            Quote::Backtick => quoted(line, &mut at, quote, '`', &mut spans, true),
            Quote::None => {
                let character = line[at..].chars().next().unwrap();
                if character.is_whitespace() {
                    let end = take_while(line, at, char::is_whitespace);
                    push(&mut spans, &line[at..end], Style::default().fg(TEXT));
                    at = end;
                } else if character == '#' && starts_shell_word(line, at) {
                    push(
                        &mut spans,
                        &line[at..],
                        Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                    );
                    at = line.len();
                } else if character == '\'' {
                    push(&mut spans, "'", Style::default().fg(GREEN));
                    at += 1;
                    *quote = Quote::Single;
                } else if character == '"' {
                    push(&mut spans, "\"", Style::default().fg(GREEN));
                    at += 1;
                    *quote = Quote::Double;
                } else if character == '`' {
                    push(&mut spans, "`", Style::default().fg(GREEN));
                    at += 1;
                    *quote = Quote::Backtick;
                } else if character == '$' {
                    let end = variable_end(line, at);
                    push(&mut spans, &line[at..end], Style::default().fg(accent()));
                    at = end;
                } else if character == '\\' {
                    let end = next_char_end(line, next_char_end(line, at));
                    push(&mut spans, &line[at..end], Style::default().fg(TEXT));
                    at = end;
                } else if character.is_ascii_alphabetic() || character == '_' {
                    let end = take_while(line, at, |c| c.is_ascii_alphanumeric() || c == '_');
                    let word = &line[at..end];
                    let next = line[end..]
                        .char_indices()
                        .find(|(_, c)| !c.is_whitespace())
                        .map_or(line.len(), |(offset, _)| end + offset);
                    let assignment = line[next..].starts_with('=');
                    let function = line[next..].starts_with("()");
                    let word_style = if assignment {
                        Style::default().fg(BLUE).add_modifier(Modifier::BOLD)
                    } else if function {
                        Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
                    } else if is_keyword(word) {
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(TEXT)
                    };
                    push(&mut spans, word, word_style);
                    at = end;
                } else if character.is_ascii_digit() && starts_number(line, at) {
                    let end = take_while(line, at, |c| {
                        c.is_ascii_hexdigit() || matches!(c, '.' | '_' | 'x' | 'X')
                    });
                    push(&mut spans, &line[at..end], Style::default().fg(YELLOW));
                    at = end;
                } else if "=(){}[];|&<>".contains(character) {
                    let end = take_while(line, at, |c| "=(){}[];|&<>".contains(c));
                    push(&mut spans, &line[at..end], Style::default().fg(DIM));
                    at = end;
                } else {
                    let end = next_char_end(line, at);
                    push(&mut spans, &line[at..end], Style::default().fg(TEXT));
                    at = end;
                }
            }
        }
    }
    Line::from(spans)
}

fn quoted(
    line: &str,
    at: &mut usize,
    quote: &mut Quote,
    terminator: char,
    spans: &mut Vec<Span<'static>>,
    interpolate: bool,
) {
    let start = *at;
    let mut cursor = start;
    while cursor < line.len() {
        let character = line[cursor..].chars().next().unwrap();
        if interpolate && character == '$' {
            push(spans, &line[start..cursor], Style::default().fg(GREEN));
            let end = variable_end(line, cursor);
            push(spans, &line[cursor..end], Style::default().fg(accent()));
            *at = end;
            return;
        }
        if interpolate && character == '\\' {
            cursor = next_char_end(line, next_char_end(line, cursor));
            continue;
        }
        cursor = next_char_end(line, cursor);
        if character == terminator {
            push(spans, &line[start..cursor], Style::default().fg(GREEN));
            *quote = Quote::None;
            *at = cursor;
            return;
        }
    }
    push(spans, &line[start..], Style::default().fg(GREEN));
    *at = line.len();
}

fn variable_end(line: &str, start: usize) -> usize {
    let next = next_char_end(line, start);
    if next >= line.len() {
        return next;
    }
    if line[next..].starts_with('{') {
        return line[next + 1..]
            .find('}')
            .map_or(line.len(), |offset| next + 1 + offset + 1);
    }
    if line[next..].starts_with("((") {
        return line[next + 2..]
            .find("))")
            .map_or(line.len(), |offset| next + 2 + offset + 2);
    }
    if line[next..].starts_with('(') {
        return next + 1;
    }
    let end = take_while(line, next, |c| c.is_ascii_alphanumeric() || c == '_');
    if end == next {
        next_char_end(line, next)
    } else {
        end
    }
}

fn starts_shell_word(line: &str, at: usize) -> bool {
    at == 0
        || line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace() || ";&|()<>".contains(c))
}

fn starts_number(line: &str, at: usize) -> bool {
    at == 0
        || line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn next_char_end(line: &str, at: usize) -> usize {
    line[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

fn take_while(line: &str, start: usize, predicate: impl Fn(char) -> bool) -> usize {
    line[start..]
        .char_indices()
        .find(|(_, c)| !predicate(*c))
        .map_or(line.len(), |(offset, _)| start + offset)
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "in"
            | "function"
            | "select"
            | "time"
            | "coproc"
            | "local"
            | "declare"
            | "readonly"
            | "export"
            | "return"
            | "break"
            | "continue"
            | "source"
    )
}

fn push(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
        last.content.to_mut().push_str(text);
    } else {
        spans.push(Span::styled(text.to_owned(), style));
    }
}
