use std::{
    collections::{HashMap, HashSet},
    ops::AddAssign,
};

use thiserror::Error;

use crate::{
    database::{Database, DatabaseError, Metadata, Stats, TermFrequency},
    indexer::{normalize_file_name, tokenizer},
    util::u64_to_f64_lossy,
};

const K1: f64 = 1.5;
const B: f64 = 0.75;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] DatabaseError),
}

// TODO: Implement the search function that takes a query string and searches for it in the indexed files. The function should return a Result type with either a list of possible files or a SearchError if the search fails.
// TODO: Implement the bm25 calculator for every indexed file and return the list of possible files ordered in descending order of their bm25 score.
pub struct Searcher<'a> {
    db: &'a Database,
}

impl<'a> Searcher<'a> {
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    // Searches for a query in the indexed files and returns the list of possible files ordered in descending order.
    pub fn search(&self, query: &str) -> Result<Vec<Metadata>, SearchError> {
        let normalized_query = normalize_file_name(query);
        let files = self.db.get_prefix_files(&normalized_query)?;
        if files.is_empty() {
            return self.search_files(&normalized_query);
        }

        Ok(files)
    }

    // Searches for files matching the query and calculates their BM25 scores.
    fn search_files(&self, query: &str) -> Result<Vec<Metadata>, SearchError> {
        let stats = self.db.get_stats()?;
        let tokens = tokenizer(query);
        let mut score: HashMap<u64, f64> = HashMap::new();
        let mut metadata: HashMap<u64, Metadata> = HashMap::new();
        let mut name_files: HashSet<u64> = HashSet::new();
        let mut files = Vec::new();

        for tok in &tokens {
            let docs = self.db.get_term_frequencies(tok)?;
            for doc in &docs {
                if !metadata.contains_key(&doc.doc_id)
                    && let Some(meta) = self.db.get_metadata(doc.doc_id)?
                {
                    metadata.insert(doc.doc_id, meta);
                }
                Self::calculate_bm25(
                    doc,
                    &stats,
                    docs.len().try_into().unwrap_or_default(),
                    &metadata,
                    &mut score,
                );
            }

            let name_docs = self.db.get_name_docs(tok)?;
            for doc in &name_docs {
                if !score.contains_key(doc)
                    && let Some(meta) = self.db.get_metadata(*doc)?
                {
                    metadata.insert(*doc, meta);
                    name_files.insert(*doc);
                }
            }
        }

        let mut scored_docs: Vec<(u64, f64)> = score.into_iter().collect();
        scored_docs.sort_by(|a, b| b.1.total_cmp(&a.1));

        for (doc_id, _) in &scored_docs {
            if let Some(metadata) = metadata.get(doc_id) {
                files.push(metadata.clone());
            }
        }

        for doc_id in name_files {
            if let Some(metadata) = metadata.get(&doc_id) {
                files.push(metadata.clone());
            }
        }

        Ok(files)
    }

    // Calculates the BM25 score for a given document and updates the score HashMap.
    fn calculate_bm25(
        doc: &TermFrequency,
        stats: &Stats,
        total_docs: u64,
        metadata: &HashMap<u64, Metadata>,
        score: &mut HashMap<u64, f64>,
    ) {
        if let Some(metadata) = metadata.get(&doc.doc_id) {
            let freq = u64_to_f64_lossy(doc.frequency);
            let denom = K1.mul_add(
                B.mul_add(
                    u64_to_f64_lossy(metadata.doc_length) / stats.avg_total_terms,
                    1.0 - B,
                ),
                freq,
            );
            let tf = freq.mul_add(K1, freq) / denom;
            let idf = ((u64_to_f64_lossy(stats.total_text_docs) - u64_to_f64_lossy(total_docs)
                + 0.5)
                / (u64_to_f64_lossy(total_docs) + 0.5))
                .ln_1p();
            let bm25_score = tf * idf;

            score
                .entry(doc.doc_id)
                .and_modify(|s| s.add_assign(bm25_score))
                .or_insert(bm25_score);
        }
    }
}
