use std::{io, sync::mpsc::Receiver, time::Duration};

use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    crossterm::{
        event::{
            self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseButton,
            MouseEventKind,
        },
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};
use thiserror::Error;

use crate::{
    database::Database,
    search::{self, Searcher},
    tui::{
        IndexingEvent,
        app::{Action, App, ExitChoice, Screen},
        opener::reveal_document,
        ui::draw,
    },
};

#[derive(Debug, Error)]
pub enum TuiErrors {
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),
    #[error("Search error: {0}")]
    SearchError(#[from] search::SearchError),
    #[error("Error opening file explorer: {0}")]
    OpenError(#[from] opener::OpenError),
}

pub fn run(db: Database, index_rx: &Receiver<IndexingEvent>) -> Result<(), TuiErrors> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(db);

    run_app(&mut terminal, &mut app, index_rx)?;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

// How often the loop wakes up even without a keypress, so `index_rx` events (indexing
// started/finished on the background thread) get drained and redrawn promptly instead of only
// on the next key.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    index_rx: &Receiver<IndexingEvent>,
) -> Result<(), TuiErrors>
where
    TuiErrors: From<B::Error>,
{
    while !app.should_quit {
        terminal.draw(|f| draw(f, app))?;

        while let Ok(event) = index_rx.try_recv() {
            app.on_indexing_event(event);
        }

        // event::poll blocks for at most POLL_INTERVAL, so the loop still wakes up regularly
        // to drain index_rx even while the user isn't pressing anything.
        if !event::poll(POLL_INTERVAL)? {
            continue;
        }

        // Handle input events and update the `app` state accordingly
        match event::read()? {
            Event::Mouse(mouse)
                if matches!(app.screen, Screen::Results)
                    && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) =>
            {
                // A click both selects and opens the row it lands on — there's no separate
                // "select via click" gesture, since Up/Down + Enter already cover that.
                if let Some(index) = app.result_index_at(mouse.column, mouse.row) {
                    app.selected = index;
                    if let Some(selected) = app.results.get(app.selected) {
                        reveal_document(selected.path.as_path())?;
                    }
                }
            }
            Event::Key(key) => {
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }

                match app.screen {
                    // The landing screen is just a query box — no results yet to navigate, so
                    // every printable key edits the query and 'q' is a plain character, not
                    // quit.
                    Screen::Home => match key.code {
                        KeyCode::Esc => app.request_exit(),
                        KeyCode::Backspace => {
                            app.query.pop();
                        }
                        KeyCode::Enter if !app.query.trim().is_empty() => {
                            let results = Searcher::new(&app.db).search(&app.query)?;
                            app.set_results(results);
                            app.screen = Screen::Results;
                        }
                        KeyCode::Char(value) => {
                            app.query.push(value);
                        }
                        _ => {}
                    },
                    Screen::Results => match key.code {
                        KeyCode::Esc => app.request_exit(),
                        KeyCode::Tab => {
                            app.toggle_action();
                        }
                        // 'q' only quits while browsing results — while typing, it's a letter
                        // like any other (queries containing "q" would otherwise be
                        // untypeable).
                        KeyCode::Char('q') if matches!(app.action, Action::Navigating) => {
                            app.request_exit();
                        }
                        KeyCode::Char(value) => match app.action {
                            Action::Searching => {
                                app.query.push(value);
                            }
                            Action::Navigating => {}
                        },
                        KeyCode::Backspace => {
                            if matches!(app.action, Action::Searching) {
                                app.query.pop();
                            }
                        }
                        KeyCode::Up => {
                            if matches!(app.action, Action::Navigating) {
                                app.select_previous();
                            }
                        }
                        KeyCode::Down => {
                            if matches!(app.action, Action::Navigating) {
                                app.select_next();
                            }
                        }
                        KeyCode::Enter => match app.action {
                            Action::Searching => {
                                let results = Searcher::new(&app.db).search(&app.query)?;
                                app.set_results(results);
                            }
                            Action::Navigating => {
                                if let Some(selected) = app.results.get(app.selected) {
                                    reveal_document(selected.path.as_path())?;
                                }
                            }
                        },
                        _ => {}
                    },
                    Screen::Exit => match key.code {
                        KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                            app.toggle_exit_choice();
                        }
                        KeyCode::Enter => match app.exit_choice {
                            ExitChoice::Yes => app.should_quit = true,
                            ExitChoice::No => app.cancel_exit(),
                        },
                        KeyCode::Char('y') => app.should_quit = true,
                        KeyCode::Char('n') | KeyCode::Esc => app.cancel_exit(),
                        _ => {}
                    },
                }
            }
            _ => {}
        }
    }
    Ok(())
}
