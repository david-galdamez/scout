// Dev-only tool: dumps the contents of the sled index so we can eyeball what
// a real indexing run wrote, without needing a query API in `Database` yet.
// Run with: cargo run --example inspect_db
use std::path::PathBuf;

use anyhow::{Context, anyhow};
use sled::{Db, IVec};

fn main() -> anyhow::Result<()> {
    let path = db_path()?;
    println!("Opening {}\n", path.display());
    let db = sled::open(&path).context("failed to open sled db")?;

    print_json_tree(&db, "metadata")?;
    print_json_tree(&db, "file_names")?;
    print_json_tree(&db, "terms")?;
    print_stats_tree(&db)?;

    Ok(())
}

fn db_path() -> anyhow::Result<PathBuf> {
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow!("could not resolve OS data dir"))?;
    Ok(data_dir.join("scout").join("index"))
}

fn print_json_tree(db: &Db, tree_name: &str) -> anyhow::Result<()> {
    let tree = db.open_tree(tree_name)?;
    println!("=== {tree_name} ({} entries) ===", tree.len());

    for entry in &tree {
        let (key, value) = entry?;
        let key_str = decode_key(tree_name, &key);
        let value_json: serde_json::Value = serde_json::from_slice(&value)?;
        println!("{key_str} => {value_json}");
    }

    println!();
    Ok(())
}

fn print_stats_tree(db: &Db) -> anyhow::Result<()> {
    let tree = db.open_tree("stats")?;
    println!("=== stats ({} entries) ===", tree.len());

    for entry in &tree {
        let (key, value) = entry?;
        let key_str = String::from_utf8_lossy(&key).into_owned();
        let count = value.as_ref().try_into().map_or(0, u64::from_be_bytes);
        println!("{key_str} => {count}");
    }

    println!();
    Ok(())
}

fn decode_key(tree_name: &str, key: &IVec) -> String {
    if tree_name == "metadata"
        && let Ok(bytes) = key.as_ref().try_into()
    {
        return u64::from_be_bytes(bytes).to_string();
    }
    String::from_utf8_lossy(key).into_owned()
}
