use std::{
    collections::{HashMap, HashSet},
    fs::{File, metadata},
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{
    database::{Database, FileType, Metadata as DatabaseMetadata},
    indexer::{
        file_walker::DirErrors,
        tokenizer::{normalize_file_name, tokenize_file_name, tokenizer},
    },
    util::{file_id, modified_secs},
};

// Resolves the doc_id an existing file was indexed under: tries the file's platform
// identifier first (stable across renames), falling back to a path lookup when the platform
// can't provide one (rare, e.g. some Windows volumes) or the doc predates the `file_ids` tree.
fn resolve_doc_id(
    path: &Path,
    file_metadata: &std::fs::Metadata,
    db: &Database,
) -> Result<u64, DirErrors> {
    let by_file_id = match file_id(file_metadata) {
        Some(id) => db.get_doc_id_by_file_id(id)?,
        None => None,
    };
    let doc_id = match by_file_id {
        Some(doc_id) => Some(doc_id),
        None => db.get_doc_id_by_path(path)?,
    };
    doc_id.ok_or_else(|| DirErrors::PathNotIndexed(path.to_path_buf()))
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
        file_id(&file_metadata),
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
        file_id(&file_metadata),
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

    let doc_id = resolve_doc_id(path, &file_metadata, db)?;
    let Some(old_metadata) = db.get_metadata(doc_id)? else {
        return Err(DirErrors::PathNotIndexed(path.to_path_buf()));
    };
    let old_file_name = normalize_file_name(
        old_metadata
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );

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
        doc_id,
        &metadata,
        &old_file_name,
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

    let doc_id = resolve_doc_id(path, &file_metadata, db)?;
    let Some(old_metadata) = db.get_metadata(doc_id)? else {
        return Err(DirErrors::PathNotIndexed(path.to_path_buf()));
    };
    let old_file_name = normalize_file_name(
        old_metadata
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );

    let file_name = normalize_file_name(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );
    let file_name_tokens = tokenize_file_name(&file_name);

    db.reindex_binary_and_image(
        doc_id,
        &metadata,
        &old_file_name,
        &file_name,
        &file_name_tokens.into_iter().collect::<HashSet<String>>(),
    )?;

    Ok(())
}
