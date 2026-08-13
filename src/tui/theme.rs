use ratatui::style::{Color, Modifier, Style};

pub const PRIMARY: Color = Color::Rgb(96, 165, 250); // blue-400: focused borders, big title
pub const ACCENT: Color = Color::Rgb(37, 99, 235); // blue-600: selected item / button background
pub const TEXT: Color = Color::Rgb(226, 232, 240); // slate-200: default readable text
pub const DIM: Color = Color::Rgb(100, 116, 139); // slate-500: unfocused / secondary text

pub fn focused(focused: bool) -> Style {
    if focused {
        Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    }
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn selected() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}
