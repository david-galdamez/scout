use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::ConfigError;

// Represents the type of file being indexed.
#[derive(Debug, Serialize, Deserialize)]
enum FileType {
    Binary,
    Image,
    Text,
}

// Represents the structure of the database used for storing file metadata.
// KEY: document ID (u64) in the sled database
#[derive(Debug, Serialize, Deserialize)]
struct Metadata {
    path: PathBuf,
    size: u64,
    modified: u64,
    kind: FileType,
}

// Represents the structure of the database used for storing file names and their associated document IDs.
// KEY: file name (String) in the sled database
#[derive(Debug, Serialize, Deserialize)]
struct FileNames {
    doc_ids: Vec<u64>,
}

type Term = String;

// Represents the structure of the database used for storing term frequencies for each document.
// KEY: term (String) in the sled database
// Vector of TermFrequency structs, each containing a document ID and the frequency of the term in that document.
#[derive(Debug, Serialize, Deserialize)]
struct TermFrequency {
    doc_id: u64,
    frequency: u64,
}

// Represents the structure of the database used for storing term frequencies for each document.
#[derive(Debug, Serialize, Deserialize)]
struct Stats {
    total_docs: u64,
}

fn default_db_path() -> Result<PathBuf, ConfigError> {
    let db_path = dirs::data_dir().ok_or(ConfigError::HomeDirNotFound)?;
    Ok(db_path.join("scout").join("index.db"))
}
