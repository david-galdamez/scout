use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    os::unix::fs::MetadataExt,
    path::Path,
};

use crate::{
    database::{Database, FileType, Metadata},
    indexer::{file_walker::DirErrors, tokenizer::tokenizer},
};

// Processes a text file, tokenizes its content, and indexes it in the database.
pub fn process_text_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
    let file = File::open(path)?;
    let reader = BufReader::new(&file);

    let mut content = String::new();

    for line in reader.lines() {
        let line = line?;
        content.push_str(&line);
        content.push('\n');
    }

    let tokens = tokenizer(&content);
    let doc_lenght = tokens.len() as u64;
    let file_metadata = file.metadata()?;

    let metadata = Metadata {
        path: path.to_path_buf(),
        size: file_metadata.size(),
        modified: file_metadata.mtime() as u64,
        kind: FileType::Text,
        doc_length: doc_lenght,
    };

    let mut counts: HashMap<&str, u64> = HashMap::new();

    tokens.iter().for_each(|tok| {
        *counts.entry(tok).or_insert(0) += 1;
    });

    db.index_document(
        metadata,
        &path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        counts,
    )?;

    Ok(())
}
