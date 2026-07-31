use std::path::{Path, PathBuf};

use sled::{Db, Error, Tree, open};
use thiserror::Error;

use crate::config::ConfigError;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("I/O error: {0}")]
    IoError(#[from] Error),
}

#[derive(Debug, Clone)]
struct Database {
    db: Db,
    metadata: Tree,
    file_names: Tree,
    terms: Tree,
}

impl Database {
    fn new(path: &Path) -> Result<Self, DatabaseError> {
        let db = open(path)?;
        let metadata_tree = db.open_tree("metadata")?;
        let file_names_tree = db.open_tree("metadata")?;
        let terms_tree = db.open_tree("metadata")?;

        Ok(Database {
            db,
            metadata: metadata_tree,
            file_names: file_names_tree,
            terms: terms_tree,
        })
    }
}

fn default_db_path() -> Result<PathBuf, ConfigError> {
    let db_path = dirs::data_dir().ok_or(ConfigError::HomeDirNotFound)?;
    Ok(db_path.join("scout").join("index.db"))
}
