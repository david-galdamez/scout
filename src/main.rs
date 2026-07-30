use std::collections::HashSet;

use anyhow::Result;

mod config;
mod database;
mod indexer;
mod search;
mod tui;

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;

    let errors = indexer::walk_dirs(
        config.indexing.include,
        config
            .indexing
            .exclude
            .into_iter()
            .collect::<HashSet<String>>(),
    )?;

    println!("Errors encountered during directory walk: {:?}", errors);

    Ok(())
}
