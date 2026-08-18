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
    config::Config,
    database::Database,
    search::Searcher,
    tui::{
        ConfigUpdate, IndexingEvent,
        app::{Action, App, ConfigFocus, ExitChoice, Screen},
        opener::{describe_open_error, reveal_document},
        ui::draw,
    },
};

#[derive(Debug, Error)]
pub enum TuiErrors {
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),
}

pub fn run(
    db: Database,
    index_rx: &Receiver<IndexingEvent>,
    config: Config,
    config_tx: crossbeam_channel::Sender<ConfigUpdate>,
) -> Result<(), TuiErrors> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(db, config, config_tx);

    let result = run_app(&mut terminal, &mut app, index_rx);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result?;

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
            app.clear_last_error();
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
                    if let Some(selected) = app.results.get(app.selected)
                        && let Err(e) = reveal_document(selected.path.as_path())
                    {
                        app.set_last_error(describe_open_error(&selected.path, &e));
                    }
                }
            }
            Event::Key(key) => {
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }

                match app.screen {
                    Screen::Home => handle_home_key(app, key.code),
                    Screen::Results => handle_results_key(app, key.code),
                    Screen::Exit => handle_exit_key(app, key.code),
                    Screen::Config => handle_config_key(app, key.code),
                    Screen::Errors => handle_errors_key(app, key.code),
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// The landing screen is just a query box — no results yet to navigate, so every printable key
// edits the query and 'q' is a plain character, not quit.
fn handle_home_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.request_exit(),
        KeyCode::F(1) => app.open_config(),
        KeyCode::F(2) => app.open_errors(),
        KeyCode::Backspace => {
            app.query.pop();
        }
        KeyCode::Enter if !app.query.trim().is_empty() => {
            let results = Searcher::new(&app.db).search(&app.query);
            match results {
                Ok(res) => {
                    app.set_results(res);
                }
                Err(e) => {
                    app.clear_last_error();
                    app.set_last_error(e.to_string());
                }
            }
            app.screen = Screen::Results;
        }
        KeyCode::Char(value) => {
            app.query.push(value);
        }
        _ => {}
    }
}

fn handle_results_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.request_exit(),
        KeyCode::F(1) => app.open_config(),
        KeyCode::F(2) => app.open_errors(),
        KeyCode::Tab => app.toggle_action(),
        // 'q' only quits while browsing results — while typing, it's a letter like any other
        // (queries containing "q" would otherwise be untypeable).
        KeyCode::Char('q') if matches!(app.action, Action::Navigating) => {
            app.request_exit();
        }
        KeyCode::Char(value) => match app.action {
            Action::Searching => app.query.push(value),
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
                let results = Searcher::new(&app.db).search(&app.query);
                match results {
                    Ok(res) => {
                        app.set_results(res);
                    }
                    Err(e) => {
                        app.clear_last_error();
                        app.set_last_error(e.to_string());
                    }
                }
            }
            Action::Navigating => {
                if let Some(selected) = app.results.get(app.selected)
                    && let Err(e) = reveal_document(selected.path.as_path())
                {
                    app.set_last_error(describe_open_error(&selected.path, &e));
                }
            }
        },
        _ => {}
    }
}

const fn handle_exit_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Left | KeyCode::Right | KeyCode::Tab => app.toggle_exit_choice(),
        KeyCode::Enter => match app.exit_choice {
            ExitChoice::Yes => app.should_quit = true,
            ExitChoice::No => app.cancel_exit(),
        },
        KeyCode::Char('y') => app.should_quit = true,
        KeyCode::Char('n') | KeyCode::Esc => app.cancel_exit(),
        _ => {}
    }
}

// `config_draft` is always `Some` while `Screen::Config` is active — set by `App::open_config`
// and only cleared by leaving it (`cancel_config`/`save_config`).
fn handle_config_key(app: &mut App, code: KeyCode) {
    let Some(draft) = app.config_draft.as_mut() else {
        return;
    };

    match draft.focus {
        ConfigFocus::List(_) => match code {
            KeyCode::Esc => app.cancel_config(),
            KeyCode::Char('s') => {
                if let Err(e) = app.save_config() {
                    app.clear_last_error();
                    app.set_last_error(e.to_string());
                }
            }
            KeyCode::Tab => draft.toggle_list(),
            KeyCode::Up => draft.select_previous(),
            KeyCode::Down => draft.select_next(),
            KeyCode::Char('a') => draft.start_adding(),
            KeyCode::Char('e') | KeyCode::Enter => draft.start_editing(),
            KeyCode::Char('d') | KeyCode::Delete => draft.remove_selected(),
            _ => {}
        },
        ConfigFocus::AddInput(_) => match code {
            KeyCode::Esc => draft.cancel_adding(),
            KeyCode::Enter => draft.confirm_adding(),
            KeyCode::Tab => draft.autocomplete(),
            KeyCode::Backspace => draft.pop_input_char(),
            KeyCode::Char(value) => draft.push_input_char(value),
            _ => {}
        },
    }
}

fn handle_errors_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Enter => app.close_errors(),
        KeyCode::Up => app.select_previous_error(),
        KeyCode::Down => app.select_next_error(),
        _ => {}
    }
}
