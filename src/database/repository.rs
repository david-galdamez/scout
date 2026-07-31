use std::path::Path;

use sled::{Db, Error as SledError, Tree, open};
use thiserror::Error;

use crate::database::schemas::{Metadata, TermFrequency};

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("I/O error: {0}")]
    Io(std::io::Error),
    #[error("Database corruption detected")]
    Corruption,
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    #[error("Collection not found")]
    CollectionNotFound,
    #[error("Internal database bug: {0}")]
    ReportableBug(String),
    #[error("Serialization error: {0}")]
    SerializeError(#[from] serde_json::Error),
}

impl From<SledError> for DatabaseError {
    fn from(err: SledError) -> Self {
        match err {
            SledError::Io(e) => DatabaseError::Io(e),
            SledError::Corruption { .. } => DatabaseError::Corruption,
            SledError::Unsupported(msg) => DatabaseError::Unsupported(msg),
            SledError::CollectionNotFound(_) => DatabaseError::CollectionNotFound,
            SledError::ReportableBug(msg) => DatabaseError::ReportableBug(msg),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Database {
    db: Db,
    metadata: Tree,
    file_names: Tree,
    terms: Tree,
    stats: Tree,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self, DatabaseError> {
        let db = open(path)?;
        let metadata_tree = db.open_tree("metadata")?;
        let file_names_tree = db.open_tree("file_names")?;
        let terms_tree = db.open_tree("terms")?;
        let stats_tree = db.open_tree("stats")?;

        Ok(Database {
            db,
            metadata: metadata_tree,
            file_names: file_names_tree,
            terms: terms_tree,
            stats: stats_tree,
        })
    }

    // Inserts metadata into the database and returns the generated document ID.
    pub fn insert_metadata(&self, metadata: &Metadata) -> Result<u64, DatabaseError> {
        //we create the document ID for the metadata entry that would be used to reference the metadata entry in the file_names and terms trees
        let doc_id = self.db.generate_id()?;
        let metadata_bytes = serde_json::to_vec(metadata)?;

        self.metadata.insert(doc_id.to_be_bytes(), metadata_bytes)?;
        Ok(doc_id)
    }

    // Inserts file name and his document ID into the database. If the file name already exists, it appends the new document ID to the existing list of document IDs
    // If the file name does not exist, it creates a new entry with the file name and the document ID
    pub fn insert_file_name(&self, file_name: &str, doc_id: u64) -> Result<(), DatabaseError> {
        let mut documents_ids: Vec<u64> = match self.file_names.get(file_name)? {
            Some(ids) => serde_json::from_slice(&ids)?,
            None => Vec::new(),
        };
        documents_ids.push(doc_id);

        let ids_bytes = serde_json::to_vec(&documents_ids)?;
        self.file_names.insert(file_name, ids_bytes)?;

        Ok(())
    }

    // Inserts a term's frequency for a given document. The caller is responsible for
    // counting occurrences within the document beforehand (one call per unique term per doc).
    pub fn insert_term(&self, term: &str, frequency: TermFrequency) -> Result<(), DatabaseError> {
        let mut term_frequencies: Vec<TermFrequency> = match self.terms.get(term)? {
            Some(freqs) => serde_json::from_slice(&freqs)?,
            None => Vec::new(),
        };
        term_frequencies.push(frequency);

        let freqs_bytes = serde_json::to_vec(&term_frequencies)?;
        self.terms.insert(term, freqs_bytes)?;

        Ok(())
    }

    // Increments the document counter in the stats tree. If the counter does not exist, it initializes it to 1.
    pub fn increment_document_counter_stats(&self) -> Result<(), DatabaseError> {
        self.stats.update_and_fetch("n_docs", |old| {
            let count = old
                .map(|bytes| {
                    u64::from_be_bytes(bytes.try_into().expect("n_docs should be 8 bytes"))
                })
                .unwrap_or(0);
            Some((count + 1).to_be_bytes().to_vec())
        })?;
        Ok(())
    }

    // Increments the total terms counter in the stats tree by a given count. If the counter does not exist, it initializes it to the given count.
    pub fn increment_total_terms_stats(&self, count: u64) -> Result<(), DatabaseError> {
        self.stats.update_and_fetch("n_terms", |old| {
            let total = old
                .map(|bytes| {
                    u64::from_be_bytes(bytes.try_into().expect("n_terms should be 8 bytes"))
                })
                .unwrap_or(0);
            Some((total + count).to_be_bytes().to_vec())
        })?;
        Ok(())
    }
}
