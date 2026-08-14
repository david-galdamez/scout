use std::{collections::HashSet, sync::mpsc, thread};

use anyhow::Result;

use crate::{config::Config, database::Database, tui::IndexingEvent};

mod config;
mod database;
mod indexer;
mod search;
mod tui;
mod util;

const REINDEX_INTERVAL: u64 = 60 * 5; // 5 minutes

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;

    let db_path = Config::default_db_path()?;
    let db = Database::new(&db_path)?;
    let thread_db = db.clone();
    let (tx, rx) = mpsc::channel::<IndexingEvent>();

    thread::spawn(move || {
        loop {
            tx.send(IndexingEvent::Started).unwrap();
            let errors = indexer::walk_dirs(
                &config.indexing.include,
                &config
                    .indexing
                    .exclude
                    .iter()
                    .map(String::from)
                    .collect::<HashSet<String>>(),
                &thread_db,
            );
            tx.send(IndexingEvent::Finished { errors }).unwrap();

            thread::sleep(std::time::Duration::from_secs(REINDEX_INTERVAL));
        }
    });

    tui::run(db, &rx)?;

    Ok(())
}
