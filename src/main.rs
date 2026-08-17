use std::{
    collections::HashSet,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    config::Config,
    database::Database,
    tui::{ConfigUpdate, IndexingEvent},
};

mod config;
mod database;
mod indexer;
mod search;
mod tui;
mod util;

const REINDEX_INTERVAL: Duration = Duration::from_mins(5);

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;

    let db_path = Config::default_db_path()?;
    let db = Database::new(&db_path)?;
    let thread_db = db.clone();
    let (tx, rx) = mpsc::channel::<IndexingEvent>();
    let (config_tx, config_rx) = mpsc::channel::<ConfigUpdate>();
    let tui_config = config.clone();

    thread::spawn(move || {
        let mut config = config;
        // Starts already-elapsed, so the first `recv_timeout` below times out immediately and
        // the walk that used to happen before the loop still happens on startup.
        let mut deadline = Instant::now();

        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match config_rx.recv_timeout(timeout) {
                Ok(update) => {
                    config = update.config;
                    if update.index_now {
                        reindex(&config, &thread_db, &tx);
                    }
                    deadline = next_deadline();
                }
                Err(RecvTimeoutError::Timeout) => {
                    reindex(&config, &thread_db, &tx);
                    deadline = next_deadline();
                }
                // The TUI (and with it, the `App` holding `config_tx`) has exited.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    tui::run(db, &rx, tui_config, config_tx)?;

    Ok(())
}

fn next_deadline() -> Instant {
    Instant::now()
        .checked_add(REINDEX_INTERVAL)
        .unwrap_or_else(Instant::now)
}

fn reindex(config: &Config, db: &Database, tx: &mpsc::Sender<IndexingEvent>) {
    let _ = tx.send(IndexingEvent::Started);
    let errors = indexer::walk_dirs(
        &config.indexing.include,
        &config
            .indexing
            .exclude
            .iter()
            .map(String::from)
            .collect::<HashSet<String>>(),
        db,
    );
    let _ = tx.send(IndexingEvent::Finished { errors });
}
