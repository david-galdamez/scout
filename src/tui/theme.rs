use ratatui::style::{Color, Modifier, Style};

use crate::database::FileType;

pub const PRIMARY: Color = Color::Rgb(56, 189, 248); // sky-400 (celeste): focused borders, big title
pub const ACCENT: Color = Color::Rgb(168, 85, 247); // purple-500: selected list item background
pub const SUCCESS: Color = Color::Rgb(74, 222, 128); // green-400: text-file tag, "stay/cancel" confirm
pub const WARNING: Color = Color::Rgb(251, 191, 36); // amber-400: image-file tag, size/extension accent
pub const DANGER: Color = Color::Rgb(248, 113, 113); // red-400: binary-file tag, "quit" confirm
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

// Highlight for the exit popup's "Yes" (quit) button when active — red reads as the
// destructive choice, distinct from `selected()`'s purple used for list navigation.
pub fn confirm() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(DANGER)
        .add_modifier(Modifier::BOLD)
}

// Highlight for the exit popup's "No" (stay) button when active — green reads as the safe
// choice.
pub fn safe() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SUCCESS)
        .add_modifier(Modifier::BOLD)
}

// Gives each `FileType` its own accent color so the kind tag in a result row is visually
// distinct at a glance, not just another dim label.
pub const fn kind_color(kind: FileType) -> Color {
    match kind {
        FileType::Text => SUCCESS,
        FileType::Image => WARNING,
        FileType::Binary => DANGER,
    }
}
