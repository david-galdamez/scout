use std::{collections::HashSet, thread};

use anyhow::Result;

use crate::{config::Config, database::Database, search::Searcher};

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

    let handle = thread::spawn(move || {
        loop {
            let _ = indexer::walk_dirs(
                &config.indexing.include,
                &config
                    .indexing
                    .exclude
                    .iter()
                    .map(|e| String::from(e))
                    .collect::<HashSet<String>>(),
                &thread_db,
            );

            thread::sleep(std::time::Duration::from_secs(REINDEX_INTERVAL));
        }
    });

    handle.join().unwrap();

    let searcher = Searcher::new(&db);
    let _ = searcher.search("git");

    Ok(())
}
