use super::settings::Accent;
use ratatui::style::Color;
use std::cell::Cell;

thread_local! { static CURRENT: Cell<Accent> = const { Cell::new(Accent::Teal) }; }
pub fn set_accent(value: Accent) {
    CURRENT.set(value);
}
pub fn accent() -> Color {
    preset_color(CURRENT.get())
}
pub fn preset_color(value: Accent) -> Color {
    match value {
        Accent::Teal => Color::Rgb(77, 201, 176),
        Accent::Blue => BLUE,
        Accent::Purple => Color::Rgb(179, 157, 219),
        Accent::Pink => Color::Rgb(215, 143, 179),
        Accent::Amber => Color::Rgb(215, 186, 125),
    }
}
pub fn selection() -> Color {
    mix(SURFACE, accent(), 0.22)
}

// Arctic-derived palette. The page background remains the terminal's default.
pub const BLUE: Color = Color::Rgb(86, 156, 214);
pub const MUTED: Color = Color::Rgb(128, 128, 128);
pub const BORDER: Color = Color::Rgb(66, 70, 77);
pub const SURFACE: Color = Color::Rgb(45, 45, 45);
pub const TEXT: Color = Color::Rgb(212, 212, 212);
pub const GREEN: Color = Color::Rgb(128, 173, 107);
pub const RED: Color = Color::Rgb(244, 71, 71);
pub const YELLOW: Color = Color::Rgb(215, 186, 125);

pub fn mix(from: Color, to: Color, amount: f32) -> Color {
    if let (Color::Rgb(a, b, c), Color::Rgb(x, y, z)) = (from, to) {
        let blend =
            |a, b| (a as f32 + (b as f32 - a as f32) * amount.clamp(0.0, 1.0)).round() as u8;
        Color::Rgb(blend(a, x), blend(b, y), blend(c, z))
    } else {
        to
    }
}
