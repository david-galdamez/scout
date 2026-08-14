use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use tui_big_text::{BigText, PixelSize};

use crate::tui::{
    app::{Action, App, ExitChoice, IndexStatus, Screen},
    format, theme,
};

// Text + style for the current `IndexStatus`, shared by the Home and Results footers so both
// screens describe the background walk the same way.
fn index_status_text(status: IndexStatus) -> (String, Style) {
    match status {
        IndexStatus::Pending => (
            "Indexación: pendiente".to_string(),
            Style::default().fg(theme::DIM),
        ),
        IndexStatus::Indexing => (
            "Indexando…".to_string(),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        IndexStatus::Done { errors: 0 } => (
            "Índice actualizado".to_string(),
            Style::default().fg(theme::SUCCESS),
        ),
        IndexStatus::Done { errors } => (
            format!("Índice actualizado · {errors} errores"),
            Style::default().fg(theme::WARNING),
        ),
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Home => draw_home(frame, app),
        Screen::Results => draw_results_screen(frame, app),
        // The confirmation overlay always sits on top of whichever screen was active before
        // it, so redraw that one underneath rather than leaving a blank frame behind it.
        Screen::Exit => match app.previous_screen {
            Screen::Results => draw_results_screen(frame, app),
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

    let hint = Paragraph::new("Enter: buscar · Esc: salir")
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
        .title("Buscar")
        .border_style(theme::focused(focused));

    let text = if app.query.is_empty() && focused {
        Line::from(Span::styled("Escribí para buscar…", theme::dim()))
    } else {
        Line::from(Span::styled(app.query.as_str(), theme::text()))
    };

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = matches!(app.action, Action::Navigating);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Resultados ({})", app.results.len()))
        .border_style(theme::focused(focused));

    // Recorded on every draw (not just when non-empty) so a click while there are no results
    // reliably misses rather than hit-testing against a stale area from a previous search.
    app.results_area = Some(area);

    if app.results.is_empty() {
        let message = if app.query.is_empty() {
            "Sin búsqueda todavía"
        } else {
            "Sin resultados"
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
                "  ·  Extension: {}  ·  Tamaño: {}  ·  modificado: {}",
                extension.as_deref().unwrap_or("sin extensión"),
                format::size(metadata.size),
                format::modified(metadata.modified),
            );

            ListItem::new(vec![
                Line::from(Span::styled(name.into_owned(), theme::text())),
                Line::from(Span::styled(path.into_owned(), theme::dim())),
                Line::from(vec![
                    Span::styled(
                        format!("Tipo: {kind}"),
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
        Action::Searching => "Enter: buscar · Tab: navegar resultados · Esc: salir",
        Action::Navigating => "↑/↓: mover selección · Tab: volver a buscar · q/Esc: salir",
    };

    let (status_text, status_style) = index_status_text(app.index_status);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Ayuda")
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
        .title("Salir")
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
        Paragraph::new("¿Salir de Scout?")
            .alignment(Alignment::Center)
            .style(theme::text()),
        question_area,
    );

    let (yes_style, no_style) = match app.exit_choice {
        ExitChoice::Yes => (theme::confirm(), theme::dim()),
        ExitChoice::No => (theme::dim(), theme::safe()),
    };
    let buttons = Line::from(vec![
        Span::styled("  Sí  ", yes_style),
        Span::raw("   "),
        Span::styled("  No  ", no_style),
    ]);
    frame.render_widget(
        Paragraph::new(buttons).alignment(Alignment::Center),
        buttons_area,
    );

    frame.render_widget(
        Paragraph::new("←/→: elegir · Enter: confirmar")
            .alignment(Alignment::Center)
            .style(theme::dim()),
        hint_area,
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
