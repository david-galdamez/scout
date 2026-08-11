use std::{
    collections::{HashMap, HashSet},
    fs::{File, Metadata, metadata},
    io::{BufRead, BufReader},
    path::Path,
    time::UNIX_EPOCH,
};

use crate::{
    database::{Database, FileType, Metadata as DatabaseMetadata},
    indexer::{
        file_walker::DirErrors,
        tokenizer::{normalize_file_name, tokenize_file_name, tokenizer},
    },
};

// Converts `Metadata::modified()` into seconds since the Unix epoch. Times before the epoch
// (clock skew, exotic filesystems) collapse to 0 rather than failing indexing over a bad mtime.
fn modified_secs(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
        .map_or(0, |d| d.as_secs())
}

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

    let metadata = DatabaseMetadata {
        path: path.to_path_buf(),
        size: file_metadata.len(),
        modified: modified_secs(&file_metadata),
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
    let file_name_tokens = tokenize_file_name(&file_name);

    db.index_document(
        &metadata,
        &file_name,
        &counts,
        &file_name_tokens.into_iter().collect::<HashSet<String>>(),
    )?;

    Ok(())
}

// Processes a binary and image and indexes it in the database.
pub fn process_binary_and_image_file(
    path: &Path,
    db: &Database,
    file_type: FileType,
) -> Result<(), DirErrors> {
    let file_metadata = metadata(path)?;

    let metadata = DatabaseMetadata {
        path: path.to_path_buf(),
        size: file_metadata.len(),
        modified: modified_secs(&file_metadata),
        kind: file_type,
        doc_length: 0,
    };

    let file_name = normalize_file_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );
    let file_name_tokens = tokenize_file_name(&file_name);

    db.index_binary_and_image(
        &metadata,
        &file_name,
        &file_name_tokens.into_iter().collect::<HashSet<String>>(),
    )?;

    Ok(())
}

// Processes a text file, tokenizes its content, and reindexes it in the database.
pub fn process_and_reindex_text_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
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

    let metadata = DatabaseMetadata {
        path: path.to_path_buf(),
        size: file_metadata.len(),
        modified: modified_secs(&file_metadata),
        kind: FileType::Text,
        doc_length: doc_lenght,
    };

    let old_metadata = match db.get_metadata_by_path(path)? {
        Some(old_metadata) => old_metadata,
        None => {
            return Err(DirErrors::PathNotIndexed(path.to_path_buf()));
        }
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
    let file_name_tokens = tokenize_file_name(&file_name);

    db.reindex_text_document(
        &metadata,
        old_metadata
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        old_metadata.doc_length,
        &file_name,
        &counts,
        &file_name_tokens.into_iter().collect::<HashSet<String>>(),
    )?;

    Ok(())
}

// Processes a binary and image and reindexes it in the database.
pub fn process_and_reindex_binary_and_images(
    path: &Path,
    db: &Database,
    file_type: FileType,
) -> Result<(), DirErrors> {
    let file_metadata = metadata(path)?;

    let metadata = DatabaseMetadata {
        path: path.to_path_buf(),
        size: file_metadata.len(),
        modified: modified_secs(&file_metadata),
        kind: file_type,
        doc_length: 0,
    };

    let old_metadata = match db.get_metadata_by_path(path)? {
        Some(old_metadata) => old_metadata,
        None => {
            return Err(DirErrors::PathNotIndexed(path.to_path_buf()));
        }
    };

    let file_name = normalize_file_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );
    let file_name_tokens = tokenize_file_name(&file_name);

    db.reindex_binary_and_image(
        &metadata,
        old_metadata
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
        &file_name,
        &file_name_tokens.into_iter().collect::<HashSet<String>>(),
    )?;

    Ok(())
}
