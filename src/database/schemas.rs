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
    path: PathBuf,
    size: u64,
    modified: u64,
    kind: FileType,
    // Number of tokens in the document, used to normalize term frequency for BM-25.
    doc_length: u64,
}

// Represents the structure of the database used for storing term frequencies for each document.
// KEY: term (String) in the sled database
// Vector of TermFrequency structs, each containing a document ID and the frequency of the term in that document.
#[derive(Debug, Serialize, Deserialize)]
pub struct TermFrequency {
    doc_id: u64,
    frequency: u64,
}
