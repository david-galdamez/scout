use anyhow::Result;

mod config;
mod database;
mod indexer;
mod search;
mod tui;

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;
    println!("{:?}", config.indexing.include);
    println!("{:?}", config.indexing.exclude);
    Ok(())
}
