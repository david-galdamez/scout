use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sled::{
    Db, Error as SledError, Tree, open,
    transaction::{
        ConflictableTransactionError, TransactionError, Transactional, TransactionalTree,
    },
};
use thiserror::Error;

use crate::{
    database::{
        FileType, Stats,
        schemas::{Metadata, TermFrequency},
    },
    util::u64_to_f64_lossy,
};

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
            SledError::Io(e) => Self::Io(e),
            SledError::Corruption { .. } => Self::Corruption,
            SledError::Unsupported(msg) => Self::Unsupported(msg),
            SledError::CollectionNotFound(_) => Self::CollectionNotFound,
            SledError::ReportableBug(msg) => Self::ReportableBug(msg),
        }
    }
}

// `Conflict` is sled's internal retry signal for the *inside* of the closure
// (`ConflictableTransactionError`) and never escapes `.transaction(...)`. What
// comes back out is `TransactionError<E>`, which only has `Abort`/`Storage`.
impl From<TransactionError<Self>> for DatabaseError {
    fn from(err: TransactionError<Self>) -> Self {
        match err {
            TransactionError::Abort(e) => e,
            TransactionError::Storage(e) => Self::from(e),
        }
    }
}

// Decodes a big-endian u64 counter stored in `stats`. A length mismatch means the tree
// holds something other than what we wrote, which we treat as corruption rather than panic.
fn decode_u64_counter(bytes: &sled::IVec) -> Result<u64, DatabaseError> {
    let array: [u8; 8] = bytes
        .as_ref()
        .try_into()
        .map_err(|_| DatabaseError::Corruption)?;
    Ok(u64::from_be_bytes(array))
}

#[derive(Debug, Clone)]
pub struct Database {
    db: Db,
    metadata: Tree,
    file_names: Tree,
    terms: Tree,
    name_terms: Tree,
    stats: Tree,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self, DatabaseError> {
        let db = open(path)?;
        let metadata_tree = db.open_tree("metadata")?;
        let file_names_tree = db.open_tree("file_names")?;
        let terms_tree = db.open_tree("terms")?;
        let name_terms_tree = db.open_tree("name_terms")?;
        let stats_tree = db.open_tree("stats")?;

        Ok(Self {
            db,
            metadata: metadata_tree,
            file_names: file_names_tree,
            terms: terms_tree,
            name_terms: name_terms_tree,
            stats: stats_tree,
        })
    }

    // Indexes a document by inserting its metadata, file name, and term frequencies into the database.
    // All writes happen inside a single sled transaction: either everything commits or nothing does.
    pub fn index_document(
        &self,
        metadata: &Metadata,
        file_name: &str,
        term_counts: &HashMap<&str, u64>,
        name_term: &HashSet<String>,
    ) -> Result<u64, DatabaseError> {
        // generate_id() is its own atomic counter, unrelated to the tree transaction below,
        // and the closure can retry on conflict — so we compute doc_id and serialize the
        // metadata once, outside the closure, instead of burning ids/doing extra work per retry.
        let doc_id = self.db.generate_id()?;
        let metadata_bytes = serde_json::to_vec(metadata)?;
        let doc_length = metadata.doc_length;

        (
            &self.metadata,
            &self.file_names,
            &self.terms,
            &self.name_terms,
            &self.stats,
        )
            .transaction(
                |(metadata_tree, file_names_tree, terms_tree, name_term_tree, stats_tree)| {
                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    let mut doc_ids: Vec<u64> = match file_names_tree.get(file_name)? {
                        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                            ConflictableTransactionError::Abort(DatabaseError::from(e))
                        })?,
                        None => Vec::new(),
                    };
                    doc_ids.push(doc_id);
                    let ids_bytes = serde_json::to_vec(&doc_ids)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    file_names_tree.insert(file_name.as_bytes(), ids_bytes)?;

                    for (term, count) in term_counts {
                        let mut freqs: Vec<TermFrequency> = match terms_tree.get(*term)? {
                            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                                ConflictableTransactionError::Abort(DatabaseError::from(e))
                            })?,
                            None => Vec::new(),
                        };
                        freqs.push(TermFrequency {
                            doc_id,
                            frequency: *count,
                        });
                        let freqs_bytes = serde_json::to_vec(&freqs).map_err(|e| {
                            ConflictableTransactionError::Abort(DatabaseError::from(e))
                        })?;
                        terms_tree.insert(term.as_bytes(), freqs_bytes)?;
                    }

                    for term in name_term {
                        let mut freqs: Vec<u64> = match name_term_tree.get(term)? {
                            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                                ConflictableTransactionError::Abort(DatabaseError::from(e))
                            })?,
                            None => Vec::new(),
                        };
                        freqs.push(doc_id);
                        let freqs_bytes = serde_json::to_vec(&freqs).map_err(|e| {
                            ConflictableTransactionError::Abort(DatabaseError::from(e))
                        })?;
                        name_term_tree.insert(term.as_bytes(), freqs_bytes)?;
                    }

                    Self::bump_stats(stats_tree, "n_text_docs", 1)?;
                    Self::bump_stats(stats_tree, "n_terms", doc_length)?;

                    Ok(())
                },
            )?;

        Ok(doc_id)
    }

    // Indexes a binary and an image by inserting its metadata, file name.
    // All writes happen inside a single sled transaction: either everything commits or nothing does.
    pub fn index_binary_and_image(
        &self,
        metadata: &Metadata,
        file_name: &str,
        name_term: &HashSet<String>,
    ) -> Result<u64, DatabaseError> {
        // generate_id() is its own atomic counter, unrelated to the tree transaction below,
        // and the closure can retry on conflict — so we compute doc_id and serialize the
        // metadata once, outside the closure, instead of burning ids/doing extra work per retry.
        let doc_id = self.db.generate_id()?;
        let metadata_bytes = serde_json::to_vec(metadata)?;

        (
            &self.metadata,
            &self.file_names,
            &self.name_terms,
            &self.stats,
        )
            .transaction(
                |(metadata_tree, file_names_tree, name_term_tree, stats_tree)| {
                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    let mut doc_ids: Vec<u64> = match file_names_tree.get(file_name)? {
                        Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                            ConflictableTransactionError::Abort(DatabaseError::from(e))
                        })?,
                        None => Vec::new(),
                    };
                    doc_ids.push(doc_id);

                    let ids_bytes = serde_json::to_vec(&doc_ids)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    file_names_tree.insert(file_name.as_bytes(), ids_bytes)?;

                    for term in name_term {
                        let mut freqs: Vec<u64> = match name_term_tree.get(term)? {
                            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                                ConflictableTransactionError::Abort(DatabaseError::from(e))
                            })?,
                            None => Vec::new(),
                        };
                        freqs.push(doc_id);
                        let freqs_bytes = serde_json::to_vec(&freqs).map_err(|e| {
                            ConflictableTransactionError::Abort(DatabaseError::from(e))
                        })?;
                        name_term_tree.insert(term.as_bytes(), freqs_bytes)?;
                    }

                    match metadata.kind {
                        FileType::Binary => Self::bump_stats(stats_tree, "n_binary_docs", 1)?,
                        FileType::Image => Self::bump_stats(stats_tree, "n_image_docs", 1)?,
                        FileType::Text => {}
                    }

                    Ok(())
                },
            )?;

        Ok(doc_id)
    }

    // Bumps a counter in the stats tree by a given amount. If the key doesn't exist, it initializes it to 0 before adding.
    fn bump_stats(
        stats_tree: &TransactionalTree,
        key: &str,
        counter: u64,
    ) -> Result<(), ConflictableTransactionError<DatabaseError>> {
        let n_docs = stats_tree
            .get(key)?
            .map(|bytes| decode_u64_counter(&bytes))
            .transpose()
            .map_err(ConflictableTransactionError::Abort)?
            .unwrap_or(0);
        stats_tree.insert(key, n_docs.saturating_add(counter).to_be_bytes().to_vec())?;
        Ok(())
    }

    // Retrieves the metadata for a given document ID. Returns None if the document ID does not exist.
    pub fn get_metadata(&self, doc_id: u64) -> Result<Option<Metadata>, DatabaseError> {
        match self.metadata.get(doc_id.to_be_bytes())? {
            Some(bytes) => {
                let metadata: Metadata = serde_json::from_slice(&bytes)?;
                Ok(Some(metadata))
            }
            None => Ok(None),
        }
    }

    // Retrieves the statistics of the indexed files, including total text documents, total terms, and average total terms. Returns a Stats struct containing the statistics.
    pub fn get_stats(&self) -> Result<Stats, DatabaseError> {
        let total_text_docs = self
            .stats
            .get("n_text_docs")?
            .map(|bytes| decode_u64_counter(&bytes))
            .transpose()?
            .unwrap_or(0);

        let total_terms = self
            .stats
            .get("n_terms")?
            .map(|bytes| decode_u64_counter(&bytes))
            .transpose()?
            .unwrap_or(0);

        let avg_total_terms = if total_text_docs > 0 {
            u64_to_f64_lossy(total_terms) / u64_to_f64_lossy(total_text_docs)
        } else {
            0.0
        };

        Ok(Stats {
            total_text_docs,
            // total_terms,
            avg_total_terms,
        })
    }

    // Retrieves all files whose names start with the given query string. Returns a vector of Metadata for the matching files.
    pub fn get_prefix_files(&self, query: &str) -> Result<Vec<Metadata>, DatabaseError> {
        let mut files = Vec::new();
        let file_names = self.file_names.scan_prefix(query);

        for item in file_names {
            let (_, value) = item?;
            let doc_ids: Vec<u64> = serde_json::from_slice(&value)?;
            for id in &doc_ids {
                if let Some(metadata) = self.get_metadata(*id)? {
                    files.push(metadata);
                }
            }
        }

        Ok(files)
    }

    // Retrieves the term frequencies for a given term. Returns a vector of TermFrequency structs, each containing a document ID and the frequency of the term in that document.
    pub fn get_term_frequencies(&self, term: &str) -> Result<Vec<TermFrequency>, DatabaseError> {
        match self.terms.get(term)? {
            Some(bytes) => {
                let term_frequencies: Vec<TermFrequency> = serde_json::from_slice(&bytes)?;
                Ok(term_frequencies)
            }
            None => Ok(Vec::new()),
        }
    }

    pub fn get_name_docs(&self, term: &str) -> Result<Vec<u64>, DatabaseError> {
        match self.name_terms.get(term)? {
            Some(bytes) => {
                let doc_ids: Vec<u64> = serde_json::from_slice(&bytes)?;
                Ok(doc_ids)
            }
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn open_db() -> (TempDir, Database) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let db = Database::new(dir.path()).expect("failed to open database");
        (dir, db)
    }

    fn sample_metadata(kind: FileType, doc_length: u64) -> Metadata {
        Metadata {
            path: PathBuf::from("/tmp/example.txt"),
            size: 1024,
            modified: 0,
            kind,
            doc_length,
        }
    }

    #[test]
    fn index_document_round_trips_metadata_and_terms() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 2);
        let term_counts = HashMap::from([("hola", 2)]);
        let name_terms = HashSet::from(["example".to_string()]);

        let doc_id = db
            .index_document(&metadata, "example.txt", &term_counts, &name_terms)
            .expect("index_document failed");

        let stored = db.get_metadata(doc_id).expect("get_metadata failed");
        assert_eq!(stored.map(|m| m.path), Some(metadata.path));

        let freqs = db
            .get_term_frequencies("hola")
            .expect("get_term_frequencies failed");
        assert_eq!(freqs.len(), 1);
        assert_eq!(freqs[0].doc_id, doc_id);
        assert_eq!(freqs[0].frequency, 2);
    }

    #[test]
    fn get_prefix_files_matches_by_normalized_name_prefix() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 1);

        db.index_document(
            &metadata,
            "reporte_final.txt",
            &HashMap::new(),
            &HashSet::new(),
        )
        .expect("index_document failed");

        let files = db
            .get_prefix_files("reporte")
            .expect("get_prefix_files failed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, metadata.path);

        let empty = db
            .get_prefix_files("otro")
            .expect("get_prefix_files failed");
        assert!(empty.is_empty());
    }

    #[test]
    fn get_name_docs_returns_docs_indexed_under_that_token() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Image, 0);
        let name_terms = HashSet::from(["foto".to_string(), "vacaciones".to_string()]);

        let doc_id = db
            .index_binary_and_image(&metadata, "foto_vacaciones.png", &name_terms)
            .expect("index_binary_and_image failed");

        let docs = db.get_name_docs("foto").expect("get_name_docs failed");
        assert_eq!(docs, vec![doc_id]);

        let none = db
            .get_name_docs("inexistente")
            .expect("get_name_docs failed");
        assert!(none.is_empty());
    }

    #[test]
    fn get_stats_reflects_indexed_text_documents() {
        let (_dir, db) = open_db();

        db.index_document(
            &sample_metadata(FileType::Text, 4),
            "a.txt",
            &HashMap::from([("uno", 1)]),
            &HashSet::new(),
        )
        .expect("index_document failed");
        db.index_document(
            &sample_metadata(FileType::Text, 6),
            "b.txt",
            &HashMap::from([("dos", 1)]),
            &HashSet::new(),
        )
        .expect("index_document failed");

        let stats = db.get_stats().expect("get_stats failed");
        assert_eq!(stats.total_text_docs, 2);
        assert!((stats.avg_total_terms - 5.0).abs() < f64::EPSILON);
    }
}
