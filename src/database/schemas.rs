use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Represents the type of file being indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileType {
    Binary,
    Image,
    Text,
}

// Represents the structure of the database used for storing file metadata.
// KEY: document ID (u64) in the sled database
#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub path: PathBuf,
    pub size: u64,
    pub modified: u64,
    pub kind: FileType,
    // Number of tokens in the document, used to normalize term frequency for BM-25.
    pub doc_length: u64,
}

// Represents the structure of the database used for storing term frequencies for each document.
// KEY: term (String) in the sled database
// Vector of TermFrequency structs, each containing a document ID and the frequency of the term in that document.
#[derive(Debug, Serialize, Deserialize)]
pub struct TermFrequency {
    pub doc_id: u64,
    pub frequency: u64,
}
