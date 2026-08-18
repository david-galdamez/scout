use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use sled::{
    Db, Error as SledError, IVec, Tree, open,
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
    util::{FileId, u64_to_f64_lossy},
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
    paths: Tree,
    document_terms: Tree,
    document_name_terms: Tree,
    file_ids: Tree,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self, DatabaseError> {
        let db = open(path)?;
        let metadata_tree = db.open_tree("metadata")?;
        let file_names_tree = db.open_tree("file_names")?;
        let terms_tree = db.open_tree("terms")?;
        let name_terms_tree = db.open_tree("name_terms")?;
        let stats_tree = db.open_tree("stats")?;
        let paths_tree = db.open_tree("paths")?;
        let document_terms_tree = db.open_tree("document_terms")?;
        let document_name_terms_tree = db.open_tree("document_name_terms")?;
        let file_ids_tree = db.open_tree("file_ids")?;

        Ok(Self {
            db,
            metadata: metadata_tree,
            file_names: file_names_tree,
            terms: terms_tree,
            name_terms: name_terms_tree,
            stats: stats_tree,
            paths: paths_tree,
            document_terms: document_terms_tree,
            document_name_terms: document_name_terms_tree,
            file_ids: file_ids_tree,
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
        file_id: Option<FileId>,
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
            &self.paths,
            &self.document_terms,
            &self.document_name_terms,
            &self.file_ids,
        )
            .transaction(
                |(
                    metadata_tree,
                    file_names_tree,
                    terms_tree,
                    name_term_tree,
                    stats_tree,
                    paths_tree,
                    document_terms_tree,
                    document_name_tree,
                    file_ids_tree,
                )| {
                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    paths_tree.insert(
                        metadata.path.to_string_lossy().as_bytes(),
                        &doc_id.to_be_bytes(),
                    )?;

                    if let Some(id) = file_id {
                        file_ids_tree.insert(&id.to_bytes(), &doc_id.to_be_bytes())?;
                    }

                    let name_terms: Vec<String> = name_term.iter().cloned().collect();
                    let name_terms_bytes = serde_json::to_vec(&name_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_name_tree.insert(&doc_id.to_be_bytes(), name_terms_bytes)?;

                    let doc_terms: Vec<String> =
                        term_counts.keys().map(|k| String::from(*k)).collect();
                    let doc_terms_byte = serde_json::to_vec(&doc_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_terms_tree.insert(&doc_id.to_be_bytes(), doc_terms_byte)?;

                    Self::insert_fresh_postings(
                        terms_tree,
                        name_term_tree,
                        file_names_tree,
                        term_counts,
                        name_term,
                        file_name,
                        doc_id,
                    )?;

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
        file_id: Option<FileId>,
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
            &self.paths,
            &self.document_name_terms,
            &self.file_ids,
        )
            .transaction(
                |(
                    metadata_tree,
                    file_names_tree,
                    name_term_tree,
                    stats_tree,
                    paths_tree,
                    document_name_tree,
                    file_ids_tree,
                )| {
                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    paths_tree.insert(
                        metadata.path.to_string_lossy().as_bytes(),
                        &doc_id.to_be_bytes(),
                    )?;

                    if let Some(id) = file_id {
                        file_ids_tree.insert(&id.to_bytes(), &doc_id.to_be_bytes())?;
                    }

                    let name_terms: Vec<String> = name_term.iter().cloned().collect();
                    let name_terms_bytes = serde_json::to_vec(&name_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_name_tree.insert(&doc_id.to_be_bytes(), name_terms_bytes)?;

                    Self::insert_name_postings(
                        name_term_tree,
                        file_names_tree,
                        name_term,
                        file_name,
                        doc_id,
                    )?;

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

    // Removes `doc_id` from the postings of its old name terms and file name entry, deleting
    // any postings list that becomes empty. Shared by `Text` and `Binary`/`Image` removal.
    fn remove_name_postings(
        name_term_tree: &TransactionalTree,
        file_names_tree: &TransactionalTree,
        old_name_terms: &[String],
        old_file_name: &str,
        doc_id: u64,
    ) -> Result<(), ConflictableTransactionError<DatabaseError>> {
        for term in old_name_terms {
            let mut doc_ids: Vec<u64> = match name_term_tree.get(term)? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
                None => Vec::new(),
            };
            doc_ids.retain(|&id| id != doc_id);
            if doc_ids.is_empty() {
                name_term_tree.remove(term.as_bytes())?;
            } else {
                let ids_bytes = serde_json::to_vec(&doc_ids)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                name_term_tree.insert(term.as_bytes(), ids_bytes)?;
            }
        }

        let mut doc_ids: Vec<u64> = match file_names_tree.get(old_file_name.as_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
            None => Vec::new(),
        };
        doc_ids.retain(|&id| id != doc_id);
        if doc_ids.is_empty() {
            file_names_tree.remove(old_file_name.as_bytes())?;
        } else {
            let ids_bytes = serde_json::to_vec(&doc_ids)
                .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
            file_names_tree.insert(old_file_name.as_bytes(), ids_bytes)?;
        }

        Ok(())
    }

    // Removes `doc_id` from the postings of its old content terms, name terms, and file name
    // entry, deleting any postings list that becomes empty. Shared by the removal half of
    // `reindex_text_document`.
    fn remove_stale_postings(
        terms_tree: &TransactionalTree,
        name_term_tree: &TransactionalTree,
        file_names_tree: &TransactionalTree,
        old_doc_terms: &[String],
        old_name_terms: &[String],
        old_file_name: &str,
        doc_id: u64,
    ) -> Result<(), ConflictableTransactionError<DatabaseError>> {
        for term in old_doc_terms {
            let mut freqs: Vec<TermFrequency> = match terms_tree.get(term)? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
                None => Vec::new(),
            };
            freqs.retain(|f| f.doc_id != doc_id);
            if freqs.is_empty() {
                terms_tree.remove(term.as_bytes())?;
            } else {
                let freqs_bytes = serde_json::to_vec(&freqs)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                terms_tree.insert(term.as_bytes(), freqs_bytes)?;
            }
        }

        Self::remove_name_postings(
            name_term_tree,
            file_names_tree,
            old_name_terms,
            old_file_name,
            doc_id,
        )
    }

    // Adds `doc_id` to the postings of its current name terms and file name entry. Shared by
    // `Text` and `Binary`/`Image` insertion.
    fn insert_name_postings(
        name_term_tree: &TransactionalTree,
        file_names_tree: &TransactionalTree,
        name_term: &HashSet<String>,
        file_name: &str,
        doc_id: u64,
    ) -> Result<(), ConflictableTransactionError<DatabaseError>> {
        let mut doc_ids: Vec<u64> = match file_names_tree.get(file_name)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
            None => Vec::new(),
        };
        doc_ids.push(doc_id);
        let ids_bytes = serde_json::to_vec(&doc_ids)
            .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
        file_names_tree.insert(file_name.as_bytes(), ids_bytes)?;

        for term in name_term {
            let mut doc_ids: Vec<u64> = match name_term_tree.get(term)? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
                None => Vec::new(),
            };
            doc_ids.push(doc_id);
            let ids_bytes = serde_json::to_vec(&doc_ids)
                .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
            name_term_tree.insert(term.as_bytes(), ids_bytes)?;
        }

        Ok(())
    }

    // Adds `doc_id` to the postings of its current content terms, name terms, and file name
    // entry. Shared by the insertion half of `reindex_text_document`.
    fn insert_fresh_postings(
        terms_tree: &TransactionalTree,
        name_term_tree: &TransactionalTree,
        file_names_tree: &TransactionalTree,
        term_counts: &HashMap<&str, u64>,
        name_term: &HashSet<String>,
        file_name: &str,
        doc_id: u64,
    ) -> Result<(), ConflictableTransactionError<DatabaseError>> {
        Self::insert_name_postings(
            name_term_tree,
            file_names_tree,
            name_term,
            file_name,
            doc_id,
        )?;

        for (term, count) in term_counts {
            let mut freqs: Vec<TermFrequency> = match terms_tree.get(*term)? {
                Some(bytes) => serde_json::from_slice(&bytes)
                    .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?,
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

        Ok(())
    }

    // Reindexes a text document by updating its metadata, file name, and term frequencies in
    // the database. `doc_id` must already be resolved by the caller (via the file's platform
    // identifier, which — unlike a path — survives a rename) rather than looked up by path
    // here, so a rename is handled the same way as a content-only change: the doc's old
    // `Metadata` (read internally, below) may have a different `path` than `metadata.path`,
    // in which case the stale `paths` entry is removed and the new one inserted.
    pub fn reindex_text_document(
        &self,
        doc_id: u64,
        metadata: &Metadata,
        old_file_name: &str,
        file_name: &str,
        term_counts: &HashMap<&str, u64>,
        name_term: &HashSet<String>,
    ) -> Result<u64, DatabaseError> {
        let old_metadata: Metadata = match self.metadata.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => return Err(DatabaseError::CollectionNotFound),
        };

        let old_name_terms: Vec<String> =
            match self.document_name_terms.get(doc_id.to_be_bytes())? {
                Some(bytes) => serde_json::from_slice(&bytes)?,
                None => Vec::new(),
            };
        let old_doc_terms: Vec<String> = match self.document_terms.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => Vec::new(),
        };

        let metadata_bytes = serde_json::to_vec(metadata)?;
        let old_path = old_metadata.path.to_string_lossy().into_owned();
        let new_path = metadata.path.to_string_lossy().into_owned();
        (
            &self.metadata,
            &self.file_names,
            &self.terms,
            &self.name_terms,
            &self.stats,
            &self.paths,
            &self.document_terms,
            &self.document_name_terms,
        )
            .transaction(
                |(
                    metadata_tree,
                    file_names_tree,
                    terms_tree,
                    name_term_tree,
                    stats_tree,
                    paths_tree,
                    document_terms_tree,
                    document_name_tree,
                )| {
                    Self::remove_stale_postings(
                        terms_tree,
                        name_term_tree,
                        file_names_tree,
                        &old_doc_terms,
                        &old_name_terms,
                        old_file_name,
                        doc_id,
                    )?;
                    Self::decrease_stat(stats_tree, "n_terms", old_metadata.doc_length)?;

                    if old_path != new_path {
                        paths_tree.remove(old_path.as_bytes())?;
                    }
                    paths_tree.insert(new_path.as_bytes(), &doc_id.to_be_bytes())?;

                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    let name_terms: Vec<String> = name_term.iter().cloned().collect();
                    let name_terms_bytes = serde_json::to_vec(&name_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_name_tree.insert(&doc_id.to_be_bytes(), name_terms_bytes)?;

                    let doc_terms: Vec<String> =
                        term_counts.keys().map(|k| String::from(*k)).collect();
                    let doc_terms_bytes = serde_json::to_vec(&doc_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_terms_tree.insert(&doc_id.to_be_bytes(), doc_terms_bytes)?;

                    Self::insert_fresh_postings(
                        terms_tree,
                        name_term_tree,
                        file_names_tree,
                        term_counts,
                        name_term,
                        file_name,
                        doc_id,
                    )?;

                    Self::bump_stats(stats_tree, "n_terms", metadata.doc_length)?;

                    Ok(())
                },
            )?;

        Ok(doc_id)
    }

    // Reindexes a binary/image document by updating its metadata and name terms in the
    // database. Same shape (and same rename handling) as `reindex_text_document`, minus
    // everything that only applies to text content (the `terms` tree, `document_terms`, and
    // the `n_terms` stat delta).
    pub fn reindex_binary_and_image(
        &self,
        doc_id: u64,
        metadata: &Metadata,
        old_file_name: &str,
        file_name: &str,
        name_term: &HashSet<String>,
    ) -> Result<u64, DatabaseError> {
        let old_metadata: Metadata = match self.metadata.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => return Err(DatabaseError::CollectionNotFound),
        };

        let old_name_terms: Vec<String> =
            match self.document_name_terms.get(doc_id.to_be_bytes())? {
                Some(bytes) => serde_json::from_slice(&bytes)?,
                None => Vec::new(),
            };

        let metadata_bytes = serde_json::to_vec(metadata)?;
        let old_path = old_metadata.path.to_string_lossy().into_owned();
        let new_path = metadata.path.to_string_lossy().into_owned();
        (
            &self.metadata,
            &self.file_names,
            &self.name_terms,
            &self.document_name_terms,
            &self.paths,
        )
            .transaction(
                |(
                    metadata_tree,
                    file_names_tree,
                    name_term_tree,
                    document_name_tree,
                    paths_tree,
                )| {
                    Self::remove_name_postings(
                        name_term_tree,
                        file_names_tree,
                        &old_name_terms,
                        old_file_name,
                        doc_id,
                    )?;

                    if old_path != new_path {
                        paths_tree.remove(old_path.as_bytes())?;
                    }
                    paths_tree.insert(new_path.as_bytes(), &doc_id.to_be_bytes())?;

                    metadata_tree.insert(&doc_id.to_be_bytes(), metadata_bytes.clone())?;

                    let name_terms: Vec<String> = name_term.iter().cloned().collect();
                    let name_terms_bytes = serde_json::to_vec(&name_terms)
                        .map_err(|e| ConflictableTransactionError::Abort(DatabaseError::from(e)))?;
                    document_name_tree.insert(&doc_id.to_be_bytes(), name_terms_bytes)?;

                    Self::insert_name_postings(
                        name_term_tree,
                        file_names_tree,
                        name_term,
                        file_name,
                        doc_id,
                    )?;

                    Ok(())
                },
            )?;

        Ok(doc_id)
    }

    pub fn delete_document(&self, file_id: FileId, file_name: &str) -> Result<(), DatabaseError> {
        let doc_id = match self.file_ids.get(file_id.to_bytes())? {
            Some(bytes) => decode_u64_counter(&bytes)?,
            None => return Err(DatabaseError::CollectionNotFound),
        };

        let metadata: Metadata = match self.metadata.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => return Err(DatabaseError::CollectionNotFound),
        };

        let name_terms: Vec<String> = match self.document_name_terms.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => Vec::new(),
        };
        let doc_terms: Vec<String> = match self.document_terms.get(doc_id.to_be_bytes())? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => Vec::new(),
        };

        let path = metadata.path.to_string_lossy().into_owned();
        (
            &self.metadata,
            &self.file_names,
            &self.terms,
            &self.name_terms,
            &self.stats,
            &self.paths,
            &self.document_terms,
            &self.document_name_terms,
            &self.file_ids,
        )
            .transaction(
                |(
                    metadata_tree,
                    file_names_tree,
                    terms_tree,
                    name_term_tree,
                    stats_tree,
                    paths_tree,
                    document_terms_tree,
                    document_name_tree,
                    file_id_tree,
                )| {
                    Self::remove_stale_postings(
                        terms_tree,
                        name_term_tree,
                        file_names_tree,
                        &doc_terms,
                        &name_terms,
                        file_name,
                        doc_id,
                    )?;

                    match metadata.kind {
                        FileType::Text => {
                            Self::decrease_stat(stats_tree, "n_text_docs", 1)?;
                            Self::decrease_stat(stats_tree, "n_terms", metadata.doc_length)?;
                        }
                        FileType::Binary => Self::decrease_stat(stats_tree, "n_binary_docs", 1)?,
                        FileType::Image => Self::decrease_stat(stats_tree, "n_image_docs", 1)?,
                    }
                    metadata_tree.remove(&doc_id.to_be_bytes())?;
                    document_name_tree.remove(&doc_id.to_be_bytes())?;
                    document_terms_tree.remove(&doc_id.to_be_bytes())?;
                    paths_tree.remove(path.as_bytes())?;
                    file_id_tree.remove(&file_id.to_bytes())?;

                    Ok(())
                },
            )?;

        Ok(())
    }

    pub fn get_all_file_ids(&self) -> Result<Vec<IVec>, DatabaseError> {
        let file_ids = self.file_ids.iter().keys().collect::<Result<_, _>>()?;

        Ok(file_ids)
    }

    pub fn get_file_modified_time(&self, path: &Path) -> Result<Option<u64>, DatabaseError> {
        match self.paths.get(path.to_string_lossy().as_bytes())? {
            Some(bytes) => {
                let doc_id: u64 = decode_u64_counter(&bytes)?;
                match self.metadata.get(doc_id.to_be_bytes())? {
                    Some(metadata_bytes) => {
                        let metadata: Metadata = serde_json::from_slice(&metadata_bytes)?;
                        Ok(Some(metadata.modified))
                    }
                    None => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    // Resolves a doc_id by the file's platform identifier (stable across renames). Returns
    // `None` if no doc was ever indexed under that identifier.
    pub fn get_doc_id_by_file_id(&self, file_id: FileId) -> Result<Option<u64>, DatabaseError> {
        match self.file_ids.get(file_id.to_bytes())? {
            Some(bytes) => Ok(Some(decode_u64_counter(&bytes)?)),
            None => Ok(None),
        }
    }

    // Resolves a doc_id by its current path. Used as a fallback when the platform can't
    // provide a stable file identifier (see `get_doc_id_by_file_id`).
    pub fn get_doc_id_by_path(&self, path: &Path) -> Result<Option<u64>, DatabaseError> {
        match self.paths.get(path.to_string_lossy().as_bytes())? {
            Some(bytes) => Ok(Some(decode_u64_counter(&bytes)?)),
            None => Ok(None),
        }
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

    // Bumps a counter in the stats tree by a given amount. If the key doesn't exist, it initializes it to 0 before adding.
    fn decrease_stat(
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
        stats_tree.insert(key, n_docs.saturating_sub(counter).to_be_bytes().to_vec())?;
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
    use crate::util::file_id;

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

    // Writes a real file to disk and returns its path plus platform `FileId` — `delete_document`
    // is keyed on `FileId`, which (unlike the rest of `Metadata`) can't be faked with a
    // made-up value since it must round-trip through `util::file_id`'s real OS metadata call.
    fn real_file(dir: &TempDir, name: &str) -> (PathBuf, FileId) {
        let path = dir.path().join(name);
        std::fs::write(&path, b"content").expect("failed to write test file");
        let fs_metadata = std::fs::metadata(&path).expect("failed to read test file metadata");
        let id = file_id(&fs_metadata).expect("file_id should be available in tests");
        (path, id)
    }

    #[test]
    fn index_document_round_trips_metadata_and_terms() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 2);
        let term_counts = HashMap::from([("hola", 2)]);
        let name_terms = HashSet::from(["example".to_string()]);

        let doc_id = db
            .index_document(&metadata, "example.txt", &term_counts, &name_terms, None)
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
            None,
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
            .index_binary_and_image(&metadata, "foto_vacaciones.png", &name_terms, None)
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
            None,
        )
        .expect("index_document failed");
        db.index_document(
            &sample_metadata(FileType::Text, 6),
            "b.txt",
            &HashMap::from([("dos", 1)]),
            &HashSet::new(),
            None,
        )
        .expect("index_document failed");

        let stats = db.get_stats().expect("get_stats failed");
        assert_eq!(stats.total_text_docs, 2);
        assert!((stats.avg_total_terms - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reindex_text_document_replaces_stale_terms_and_reuses_doc_id() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 1);

        let doc_id = db
            .index_document(
                &metadata,
                "example.txt",
                &HashMap::from([("manzana", 1)]),
                &HashSet::new(),
                None,
            )
            .expect("index_document failed");

        let new_metadata = sample_metadata(FileType::Text, 1);
        let reindexed_id = db
            .reindex_text_document(
                doc_id,
                &new_metadata,
                "example.txt",
                "example.txt",
                &HashMap::from([("pera", 1)]),
                &HashSet::new(),
            )
            .expect("reindex_text_document failed");

        assert_eq!(reindexed_id, doc_id);
        assert!(
            db.get_term_frequencies("manzana")
                .expect("get_term_frequencies failed")
                .is_empty()
        );
        let freqs = db
            .get_term_frequencies("pera")
            .expect("get_term_frequencies failed");
        assert_eq!(freqs.len(), 1);
        assert_eq!(freqs[0].doc_id, doc_id);
    }

    #[test]
    fn reindex_text_document_moves_file_name_entry_on_rename() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 1);

        let doc_id = db
            .index_document(
                &metadata,
                "old_name.txt",
                &HashMap::new(),
                &HashSet::new(),
                None,
            )
            .expect("index_document failed");

        db.reindex_text_document(
            doc_id,
            &metadata,
            "old_name.txt",
            "new_name.txt",
            &HashMap::new(),
            &HashSet::new(),
        )
        .expect("reindex_text_document failed");

        assert!(
            db.get_prefix_files("old_name")
                .expect("get_prefix_files failed")
                .is_empty()
        );
        let files = db
            .get_prefix_files("new_name")
            .expect("get_prefix_files failed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, metadata.path);
    }

    #[test]
    fn reindex_text_document_does_not_duplicate_file_names_entry_when_name_unchanged() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 1);

        let doc_id = db
            .index_document(
                &metadata,
                "same.txt",
                &HashMap::new(),
                &HashSet::new(),
                None,
            )
            .expect("index_document failed");

        db.reindex_text_document(
            doc_id,
            &metadata,
            "same.txt",
            "same.txt",
            &HashMap::new(),
            &HashSet::new(),
        )
        .expect("reindex_text_document failed");

        let files = db
            .get_prefix_files("same")
            .expect("get_prefix_files failed");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn reindex_text_document_adjusts_stats_by_delta() {
        let (_dir, db) = open_db();

        let doc_id = db
            .index_document(
                &sample_metadata(FileType::Text, 4),
                "a.txt",
                &HashMap::new(),
                &HashSet::new(),
                None,
            )
            .expect("index_document failed");
        db.index_document(
            &sample_metadata(FileType::Text, 6),
            "b.txt",
            &HashMap::new(),
            &HashSet::new(),
            None,
        )
        .expect("index_document failed");

        // avg is (4 + 6) / 2 = 5 before reindexing.
        let stats = db.get_stats().expect("get_stats failed");
        assert!((stats.avg_total_terms - 5.0).abs() < f64::EPSILON);

        db.reindex_text_document(
            doc_id,
            &sample_metadata(FileType::Text, 10),
            "a.txt",
            "a.txt",
            &HashMap::new(),
            &HashSet::new(),
        )
        .expect("reindex_text_document failed");

        // avg is now (10 + 6) / 2 = 8 after growing the first doc from 4 to 10 terms.
        let stats = db.get_stats().expect("get_stats failed");
        assert_eq!(stats.total_text_docs, 2);
        assert!((stats.avg_total_terms - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reindex_text_document_errors_when_doc_id_was_never_indexed() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Text, 1);

        let result = db.reindex_text_document(
            999,
            &metadata,
            "example.txt",
            "example.txt",
            &HashMap::new(),
            &HashSet::new(),
        );

        assert!(matches!(result, Err(DatabaseError::CollectionNotFound)));
    }

    #[test]
    fn reindex_binary_and_image_replaces_stale_name_terms_and_reuses_doc_id() {
        let (_dir, db) = open_db();
        let metadata = sample_metadata(FileType::Image, 0);

        let doc_id = db
            .index_binary_and_image(
                &metadata,
                "foto_vacaciones.png",
                &HashSet::from(["foto".to_string(), "vacaciones".to_string()]),
                None,
            )
            .expect("index_binary_and_image failed");

        let reindexed_id = db
            .reindex_binary_and_image(
                doc_id,
                &metadata,
                "foto_vacaciones.png",
                "foto_playa.png",
                &HashSet::from(["foto".to_string(), "playa".to_string()]),
            )
            .expect("reindex_binary_and_image failed");

        assert_eq!(reindexed_id, doc_id);
        assert!(
            db.get_name_docs("vacaciones")
                .expect("get_name_docs failed")
                .is_empty()
        );
        assert_eq!(
            db.get_name_docs("playa").expect("get_name_docs failed"),
            vec![doc_id]
        );
        assert_eq!(
            db.get_name_docs("foto").expect("get_name_docs failed"),
            vec![doc_id]
        );
        assert!(
            db.get_prefix_files("foto_vacaciones")
                .expect("get_prefix_files failed")
                .is_empty()
        );
        assert_eq!(
            db.get_prefix_files("foto_playa")
                .expect("get_prefix_files failed")
                .len(),
            1
        );
    }

    #[test]
    fn delete_document_removes_text_doc_from_every_tree() {
        let (dir, db) = open_db();
        let (path, id) = real_file(&dir, "example.txt");
        let metadata = Metadata {
            path: path.clone(),
            size: 7,
            modified: 0,
            kind: FileType::Text,
            doc_length: 1,
        };

        let doc_id = db
            .index_document(
                &metadata,
                "example.txt",
                &HashMap::from([("hola", 1)]),
                &HashSet::from(["example".to_string()]),
                Some(id),
            )
            .expect("index_document failed");

        db.delete_document(id, "example.txt")
            .expect("delete_document failed");

        assert!(
            db.get_metadata(doc_id)
                .expect("get_metadata failed")
                .is_none()
        );
        assert!(
            db.get_term_frequencies("hola")
                .expect("get_term_frequencies failed")
                .is_empty()
        );
        assert!(
            db.get_name_docs("example")
                .expect("get_name_docs failed")
                .is_empty()
        );
        assert!(
            db.get_prefix_files("example")
                .expect("get_prefix_files failed")
                .is_empty()
        );
        assert!(
            db.get_doc_id_by_file_id(id)
                .expect("get_doc_id_by_file_id failed")
                .is_none()
        );
        assert!(
            db.get_doc_id_by_path(&path)
                .expect("get_doc_id_by_path failed")
                .is_none()
        );
    }

    #[test]
    fn delete_document_decrements_text_doc_stats() {
        let (dir, db) = open_db();
        let (path_a, id_a) = real_file(&dir, "a.txt");
        let (path_b, id_b) = real_file(&dir, "b.txt");

        db.index_document(
            &Metadata {
                path: path_a,
                size: 1,
                modified: 0,
                kind: FileType::Text,
                doc_length: 4,
            },
            "a.txt",
            &HashMap::new(),
            &HashSet::new(),
            Some(id_a),
        )
        .expect("index_document failed");
        db.index_document(
            &Metadata {
                path: path_b,
                size: 1,
                modified: 0,
                kind: FileType::Text,
                doc_length: 6,
            },
            "b.txt",
            &HashMap::new(),
            &HashSet::new(),
            Some(id_b),
        )
        .expect("index_document failed");

        db.delete_document(id_a, "a.txt")
            .expect("delete_document failed");

        let stats = db.get_stats().expect("get_stats failed");
        assert_eq!(stats.total_text_docs, 1);
        assert!((stats.avg_total_terms - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn delete_document_decrements_image_doc_count_not_terms() {
        let (dir, db) = open_db();
        let (path, id) = real_file(&dir, "foto.png");
        let metadata = Metadata {
            path,
            size: 1,
            modified: 0,
            kind: FileType::Image,
            doc_length: 0,
        };

        db.index_binary_and_image(
            &metadata,
            "foto.png",
            &HashSet::from(["foto".to_string()]),
            Some(id),
        )
        .expect("index_binary_and_image failed");

        db.delete_document(id, "foto.png")
            .expect("delete_document failed");

        assert!(
            db.get_name_docs("foto")
                .expect("get_name_docs failed")
                .is_empty()
        );
        // Deleting an image doc must not touch n_terms — it never contributed to it.
        let stats = db.get_stats().expect("get_stats failed");
        assert_eq!(stats.total_text_docs, 0);
        assert!((stats.avg_total_terms - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn delete_document_errors_for_unknown_file_id() {
        let (dir, db) = open_db();
        let (_path, id) = real_file(&dir, "never_indexed.txt");

        let result = db.delete_document(id, "never_indexed.txt");

        assert!(matches!(result, Err(DatabaseError::CollectionNotFound)));
    }
}
