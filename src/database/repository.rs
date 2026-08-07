use std::{collections::HashMap, path::Path};

use sled::{
    Db, Error as SledError, Tree, open,
    transaction::{ConflictableTransactionError, TransactionError, Transactional},
};
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
    stats: Tree,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self, DatabaseError> {
        let db = open(path)?;
        let metadata_tree = db.open_tree("metadata")?;
        let file_names_tree = db.open_tree("file_names")?;
        let terms_tree = db.open_tree("terms")?;
        let stats_tree = db.open_tree("stats")?;

        Ok(Self {
            db,
            metadata: metadata_tree,
            file_names: file_names_tree,
            terms: terms_tree,
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
    ) -> Result<u64, DatabaseError> {
        // generate_id() is its own atomic counter, unrelated to the tree transaction below,
        // and the closure can retry on conflict — so we compute doc_id and serialize the
        // metadata once, outside the closure, instead of burning ids/doing extra work per retry.
        let doc_id = self.db.generate_id()?;
        let metadata_bytes = serde_json::to_vec(metadata)?;
        let doc_length = metadata.doc_length;

        (&self.metadata, &self.file_names, &self.terms, &self.stats).transaction(
            |(metadata_tree, file_names_tree, terms_tree, stats_tree)| {
                metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                let mut doc_ids: Vec<u64> = match file_names_tree.get(file_name)? {
                    Some(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
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
                    let freqs_bytes = serde_json::to_vec(&freqs)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    terms_tree.insert(term.as_bytes(), freqs_bytes)?;
                }

                let n_docs = stats_tree
                    .get("n_docs")?
                    .map(|bytes| decode_u64_counter(&bytes))
                    .transpose()
                    .map_err(ConflictableTransactionError::Abort)?
                    .unwrap_or(0);
                stats_tree.insert("n_docs", n_docs.saturating_add(1).to_be_bytes().to_vec())?;

                let n_terms = stats_tree
                    .get("n_terms")?
                    .map(|bytes| decode_u64_counter(&bytes))
                    .transpose()
                    .map_err(ConflictableTransactionError::Abort)?
                    .unwrap_or(0);
                stats_tree.insert(
                    "n_terms",
                    n_terms.saturating_add(doc_length).to_be_bytes().to_vec(),
                )?;

                Ok(())
            },
        )?;

        Ok(doc_id)
    }
}
