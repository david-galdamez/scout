use std::collections::HashSet;

use anyhow::Result;

use crate::{config::Config, database::Database, search::Searcher};

mod config;
mod database;
mod indexer;
mod search;
mod tui;
mod util;

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;

    let db_path = Config::default_db_path()?;
    let db = Database::new(&db_path)?;

    let errors = indexer::walk_dirs(
        config.indexing.include,
        &config
            .indexing
            .exclude
            .into_iter()
            .collect::<HashSet<String>>(),
        &db,
    );

    let searcher = Searcher::new(&db);
    let _ = searcher.search("git");

    println!("Errors encountered during directory walk: {errors:?}");

    Ok(())
}
