use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use tui_big_text::{BigText, PixelSize};

use crate::tui::{
    app::{
        Action, App, ConfigDraft, ConfigFocus, ConfigList, ExitChoice, IndexStatus, Screen,
        suggestion_label,
    },
    format, theme,
};

// Text + style for the current `IndexStatus`, shared by the Home and Results footers so both
// screens describe the background walk the same way.
fn index_status_text(status: IndexStatus) -> (String, Style) {
    match status {
        IndexStatus::Pending => (
            "Indexing: pending".to_string(),
            Style::default().fg(theme::DIM),
        ),
        IndexStatus::Indexing => (
            "Indexing…".to_string(),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        IndexStatus::Done { errors: 0 } => (
            "Index up to date".to_string(),
            Style::default().fg(theme::SUCCESS),
        ),
        IndexStatus::Done { errors } => (
            format!("Index up to date · {errors} errors"),
            Style::default().fg(theme::WARNING),
        ),
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Home => draw_home(frame, app),
        Screen::Results => draw_results_screen(frame, app),
        Screen::Config => draw_config_screen(frame, app),
        Screen::Errors => draw_errors_screen(frame, app),
        // The confirmation overlay always sits on top of whichever screen was active before
        // it, so redraw that one underneath rather than leaving a blank frame behind it.
        Screen::Exit => match app.previous_screen {
            Screen::Results => draw_results_screen(frame, app),
            Screen::Config => draw_config_screen(frame, app),
            Screen::Errors => draw_errors_screen(frame, app),
            Screen::Home | Screen::Exit => draw_home(frame, app),
        },
    }

    if matches!(app.screen, Screen::Exit) {
        draw_exit_popup(frame, app);
    }
}

// The landing screen: a big "SCOUT" wordmark and a centered search box, Google-style.
fn draw_home(frame: &mut Frame, app: &App) {
    let [_, title_area, input_area, hint_area, status_area, _] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(frame.area());

    let title = BigText::builder()
        .pixel_size(PixelSize::Full)
        .style(Style::default().fg(theme::PRIMARY))
        .centered()
        .lines(vec!["SCOUT".into()])
        .build();
    frame.render_widget(title, title_area);

    draw_input_box(frame, app, centered_rect(60, 100, input_area), true);

    let hint = Paragraph::new("Enter: search · F1: settings · F2: errors · Esc: quit")
        .alignment(Alignment::Center)
        .style(theme::dim());
    frame.render_widget(hint, centered_rect(60, 100, hint_area));

    let (status_text, status_style) = index_status_text(app.index_status);
    let status =
        Paragraph::new(Span::styled(status_text, status_style)).alignment(Alignment::Center);
    frame.render_widget(status, centered_rect(60, 100, status_area));
}

// The results screen: "SCOUT" pinned top-left, search box top-right, results list below, and
// the same navigation footer as before.
fn draw_results_screen(frame: &mut Frame, app: &mut App) {
    let [top_area, results_area, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .areas(frame.area());

    let [title_area, input_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(18), Constraint::Min(0)])
        .areas(top_area);

    let title = Paragraph::new("SCOUT")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::dim())
                .padding(ratatui::widgets::Padding::horizontal(1)),
        );
    frame.render_widget(title, title_area);

    let focused = matches!(app.action, Action::Searching);
    draw_input_box(frame, app, input_area, focused);

    draw_results(frame, app, results_area);
    draw_footer(frame, app, footer_area);
}

fn draw_input_box(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Search")
        .border_style(theme::focused(focused));

    let text = if app.query.is_empty() && focused {
        Line::from(Span::styled("Type to search…", theme::dim()))
    } else {
        Line::from(Span::styled(app.query.as_str(), theme::text()))
    };

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = matches!(app.action, Action::Navigating);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Results ({})", app.results.len()))
        .border_style(theme::focused(focused));

    // Recorded on every draw (not just when non-empty) so a click while there are no results
    // reliably misses rather than hit-testing against a stale area from a previous search.
    app.results_area = Some(area);

    if app.results.is_empty() {
        let message = if app.query.is_empty() {
            "No search yet"
        } else {
            "No results"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(message, theme::dim())).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|metadata| {
            let name = metadata
                .path
                .file_name()
                .map_or_else(|| metadata.path.to_string_lossy(), |n| n.to_string_lossy());
            let path = metadata.path.to_string_lossy();
            let kind = format::kind_label(metadata.kind);
            let extension = format::extension(&metadata.path);
            let details = format!(
                "  ·  Extension: {}  ·  Size: {}  ·  modified: {}",
                extension.as_deref().unwrap_or("no extension"),
                format::size(metadata.size),
                format::modified(metadata.modified),
            );

            ListItem::new(vec![
                Line::from(Span::styled(name.into_owned(), theme::text())),
                Line::from(Span::styled(path.into_owned(), theme::dim())),
                Line::from(vec![
                    Span::styled(
                        format!("Kind: {kind}"),
                        Style::default()
                            .fg(theme::kind_color(metadata.kind))
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::styled(details, theme::dim()),
                ]),
                // Line::raw(""),
            ])
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(theme::selected());

    app.results_list_state
        .select(focused.then_some(app.selected));

    frame.render_stateful_widget(list, area, &mut app.results_list_state);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let help = match app.action {
        Action::Searching => {
            "Enter: search · Tab: navigate results · F1: settings · F2: errors · Esc: quit"
        }
        Action::Navigating => {
            "↑/↓: move selection · Tab: back to search · F1: settings · F2: errors · q/Esc: quit"
        }
    };

    let (status_text, status_style) = index_status_text(app.index_status);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Help")
        .title_top(Line::from(Span::styled(status_text, status_style)).right_aligned())
        .border_style(theme::dim());

    frame.render_widget(
        Paragraph::new(Span::styled(help, theme::dim())).block(block),
        area,
    );
}

fn draw_exit_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect_fixed(36, 9, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Quit")
        .border_style(theme::focused(true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [question_area, _, buttons_area, _, hint_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .areas(inner);

    frame.render_widget(
        Paragraph::new("Quit Scout?")
            .alignment(Alignment::Center)
            .style(theme::text()),
        question_area,
    );

    let (yes_style, no_style) = match app.exit_choice {
        ExitChoice::Yes => (theme::confirm(), theme::dim()),
        ExitChoice::No => (theme::dim(), theme::safe()),
    };
    let buttons = Line::from(vec![
        Span::styled("  Yes  ", yes_style),
        Span::raw("   "),
        Span::styled("  No  ", no_style),
    ]);
    frame.render_widget(
        Paragraph::new(buttons).alignment(Alignment::Center),
        buttons_area,
    );

    frame.render_widget(
        Paragraph::new("←/→: choose · Enter: confirm")
            .alignment(Alignment::Center)
            .style(theme::dim()),
        hint_area,
    );
}

// The config screen: `include` and `exclude` side by side, each navigable independently, with
// an inline text input that appears in place of the list's own hint line while adding an entry.
fn draw_config_screen(frame: &mut Frame, app: &App) {
    let Some(draft) = app.config_draft.as_ref() else {
        return;
    };

    let [title_area, lists_area, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .areas(frame.area());

    let title = Paragraph::new("Settings")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::dim()),
        );
    frame.render_widget(title, title_area);

    let [include_area, exclude_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(lists_area);

    draw_config_list(
        frame,
        draft,
        ConfigList::Include,
        "Include",
        draft
            .include
            .iter()
            .map(|p| p.to_string_lossy().into_owned()),
        include_area,
    );
    draw_config_list(
        frame,
        draft,
        ConfigList::Exclude,
        "Exclude",
        draft.exclude.iter().cloned(),
        exclude_area,
    );

    let help = match draft.focus {
        ConfigFocus::List(_) => {
            "Tab: switch list · ↑/↓: move · a: add · e/Enter: edit · d: remove · s: save · Esc: cancel"
        }
        ConfigFocus::AddInput(ConfigList::Include) => {
            "Enter: confirm · Tab: autocomplete · Esc: cancel"
        }
        ConfigFocus::AddInput(ConfigList::Exclude) => "Enter: confirm · Esc: cancel",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(help, theme::dim())).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Help")
                .border_style(theme::dim()),
        ),
        footer_area,
    );
}

fn draw_config_list(
    frame: &mut Frame,
    draft: &ConfigDraft,
    list: ConfigList,
    title: &str,
    entries: impl Iterator<Item = String>,
    area: Rect,
) {
    let focused = matches!(
        draft.focus,
        ConfigFocus::List(active) | ConfigFocus::AddInput(active) if active == list
    );

    let items: Vec<ListItem> = entries
        .enumerate()
        .map(|(index, entry)| {
            let selected =
                focused && matches!(draft.focus, ConfigFocus::List(_)) && index == draft.selected;
            let style = if selected {
                theme::selected()
            } else {
                theme::text()
            };
            ListItem::new(Line::from(Span::styled(entry, style)))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(theme::focused(focused));

    frame.render_widget(List::new(items).block(block), area);

    if matches!(draft.focus, ConfigFocus::AddInput(active) if active == list) {
        let width = area.width.saturating_sub(2).max(1);
        let prefix = if draft.editing_index.is_some() {
            "Edit> "
        } else {
            "Add> "
        };
        let text = format!("{prefix}{}", draft.input);

        // How many rows the input needs once wrapped to `width`, capped to whatever's left
        // inside the block's borders (minus one row reserved for the suggestions line above
        // it) so a very long path grows the input box instead of overflowing it.
        let content_rows = u16::try_from(text.chars().count())
            .unwrap_or(u16::MAX)
            .div_ceil(width)
            .max(1);
        let max_input_height = area.height.saturating_sub(3).max(1);
        let input_height = content_rows.min(max_input_height);

        let input_area = Rect {
            x: area.x.saturating_add(1),
            y: area
                .y
                .saturating_add(area.height.saturating_sub(1).saturating_sub(input_height)),
            width,
            height: input_height,
        };
        frame.render_widget(Clear, input_area);
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme::focused(true))).wrap(Wrap { trim: false }),
            input_area,
        );

        // Only room for the suggestions line if the (now possibly multi-row) input box hasn't
        // grown all the way up to the block's top border.
        if !draft.suggestions.is_empty() && input_area.y > area.y.saturating_add(1) {
            let suggestions_area = Rect {
                x: area.x.saturating_add(1),
                y: input_area.y.saturating_sub(1),
                width,
                height: 1,
            };
            let line = draft
                .suggestions
                .iter()
                .map(|path| suggestion_label(path))
                .collect::<Vec<_>>()
                .join("  ");
            frame.render_widget(Clear, suggestions_area);
            frame.render_widget(
                Paragraph::new(Span::styled(line, theme::dim())),
                suggestions_area,
            );
        }
    }
}

// Lists the per-file errors from the most recently finished walk (permission denials, symlink
// loops, I/O errors, etc.) — each entry's path alongside the error's `Display` text.
fn draw_errors_screen(frame: &mut Frame, app: &App) {
    let [title_area, list_area, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .areas(frame.area());

    let title = Paragraph::new(format!("Errors ({})", app.errors.len()))
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::dim()),
        );
    frame.render_widget(title, title_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::focused(true));

    if app.errors.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("No errors from the last walk", theme::dim())).block(block),
            list_area,
        );
    } else {
        let items: Vec<ListItem> = app
            .errors
            .iter()
            .map(|(path, error)| {
                ListItem::new(vec![
                    Line::from(Span::styled(
                        path.to_string_lossy().into_owned(),
                        theme::text(),
                    )),
                    Line::from(Span::styled(error.to_string(), theme::dim())),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(theme::selected());

        let mut state = ratatui::widgets::ListState::default();
        state.select(Some(app.errors_selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    frame.render_widget(
        Paragraph::new(Span::styled("↑/↓: move · Enter/Esc: back", theme::dim())).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Help")
                .border_style(theme::dim()),
        ),
        footer_area,
    );
}

/// helper function to create a centered rect using up certain percentage of the available rect `r`
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let side_y = 100u16.saturating_sub(percent_y) / 2;
    let side_x = 100u16.saturating_sub(percent_x) / 2;

    // Cut the given rectangle into three vertical pieces
    let [_, middle, _] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(side_y),
            Constraint::Percentage(percent_y),
            Constraint::Percentage(side_y),
        ])
        .areas(r);

    // Then cut the middle vertical piece into three width-wise pieces, returning the middle one
    let [_, middle, _] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(side_x),
            Constraint::Percentage(percent_x),
            Constraint::Percentage(side_x),
        ])
        .areas(middle);

    middle
}

// Centers a fixed-size (rather than percentage-of-parent) rect — used for the exit popup so it
// stays a small, readable box instead of scaling up on large terminals.
fn centered_rect_fixed(width: u16, height: u16, r: Rect) -> Rect {
    let width = width.min(r.width);
    let height = height.min(r.height);
    let x = r.x.saturating_add(r.width.saturating_sub(width) / 2);
    let y = r.y.saturating_add(r.height.saturating_sub(height) / 2);

    Rect {
        x,
        y,
        width,
        height,
    }
}
