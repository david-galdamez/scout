use std::{
    collections::HashMap,
    fs::{File, metadata},
    io::{BufRead, BufReader},
    os::unix::fs::MetadataExt,
    path::Path,
};

use crate::{
    database::{Database, FileType, Metadata},
    indexer::{
        file_walker::DirErrors,
        tokenizer::{normalize_file_name, tokenizer},
    },
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
    let doc_lenght: u64 = tokens.len().try_into().unwrap_or_default();
    let file_metadata = file.metadata()?;

    let metadata = Metadata {
        path: path.to_path_buf(),
        size: file_metadata.size(),
        modified: file_metadata.mtime().cast_unsigned(),
        kind: FileType::Text,
        doc_length: doc_lenght,
    };

    let mut counts: HashMap<&str, u64> = HashMap::new();

    for tok in &tokens {
        let count = counts.entry(tok.as_str()).or_insert(0);
        *count = count.saturating_add(1);
    }

    let file_name = normalize_file_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );

    db.index_document(&metadata, &file_name, &counts)?;

    Ok(())
}

// Processes a binary and image and indexes it in the database.
pub fn process_binary_and_image_file(
    path: &Path,
    db: &Database,
    file_type: FileType,
) -> Result<(), DirErrors> {
    let file_metadata = metadata(path)?;

    let metadata = Metadata {
        path: path.to_path_buf(),
        size: file_metadata.size(),
        modified: file_metadata.mtime().cast_unsigned(),
        kind: file_type,
        doc_length: 0,
    };

    let file_name = normalize_file_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );

    db.index_binary_and_image(&metadata, &file_name)?;

    Ok(())
}
