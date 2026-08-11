use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use sled::IVec;
use thiserror::Error;
use walkdir::WalkDir;

use crate::{
    database::{Database, FileType},
    indexer::{
        classifier::classify,
        processors::{
            process_and_reindex_binary_and_images, process_and_reindex_text_file,
            process_binary_and_image_file, process_text_file,
        },
        tokenizer::normalize_file_name,
    },
    util::{FileId, file_id, modified_secs},
};

#[derive(Debug, Error)]
pub enum DirErrors {
    #[error("Not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("Directory doesnt exist: {0}")]
    DirectoryDoesntExist(PathBuf),
    #[error("Permission denied")]
    PermissionDenied,
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Symlink loop detected")]
    SymlinkLoop,
    #[error("Database error: {0}")]
    DatabaseError(#[from] crate::database::DatabaseError),
    #[error("File not indexed: {0}")]
    PathNotIndexed(PathBuf),
}

// Whether a file is being seen for the first time, is unchanged since the last time it was
// indexed, or was modified since then.
enum IndexState {
    New,
    Unchanged,
    Modified,
}

// Walks through the provided directories, classifies files, and processes them accordingly. Returns a vector of errors encountered during the walk.
pub fn walk_dirs(
    dirs: Vec<PathBuf>,
    exclude: &HashSet<String>,
    db: &Database,
) -> Vec<(PathBuf, DirErrors)> {
    let mut errors = Vec::new();
    let mut visited_file_ids: HashSet<IVec> = HashSet::new();

    for dir in dirs {
        if let Err(e) = validate_dir(&dir) {
            errors.push((dir.clone(), e));
            continue;
        }

        for entry in WalkDir::new(dir)
            .into_iter()
            .filter_entry(|e| !exclude.contains(e.file_name().to_str().unwrap_or("")))
        {
            match entry {
                Ok(entry) => {
                    if entry.file_type().is_file() {
                        match index_state(entry.path(), db, &mut visited_file_ids) {
                            Ok(IndexState::Unchanged) => {}
                            Ok(IndexState::New) => {
                                if let Err(e) = index_file(entry.path(), db) {
                                    errors.push((entry.path().to_path_buf(), e));
                                }
                            }
                            Ok(IndexState::Modified) => {
                                if let Err(e) = reindex_file(entry.path(), db) {
                                    errors.push((entry.path().to_path_buf(), e));
                                }
                            }
                            Err(e) => errors.push((entry.path().to_path_buf(), e)),
                        }
                    }
                }
                Err(e) => {
                    let path = e
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();

                    let loop_path = e
                        .loop_ancestor()
                        .unwrap_or_else(|| Path::new(""))
                        .to_path_buf();

                    match e.into_io_error() {
                        Some(inner) if inner.kind() == std::io::ErrorKind::PermissionDenied => {
                            errors.push((PathBuf::from(&path), DirErrors::PermissionDenied));
                        }
                        Some(inner) => {
                            errors
                                .push((Path::new(&path).to_path_buf(), DirErrors::IoError(inner)));
                        }
                        None => {
                            errors.push((loop_path, DirErrors::SymlinkLoop));
                        }
                    }
                }
            }
        }
    }

    let file_ids = match db.get_all_file_ids() {
        Ok(ids) => ids,
        Err(e) => {
            errors.push((PathBuf::new(), DirErrors::DatabaseError(e)));
            return errors;
        }
    };

    for raw_id in file_ids {
        if visited_file_ids.contains(&raw_id) {
            continue;
        }

        // A key of the wrong length would mean the tree holds something other than what we
        // wrote — skip it rather than fail the whole run over one bad entry.
        let Some(id) = FileId::from_bytes(raw_id.as_ref()) else {
            continue;
        };

        if let Err(e) = prune_stale_file(db, id) {
            errors.push((PathBuf::new(), e));
        }
    }

    errors
}

// Removes the doc indexed under `id` when the file it points to wasn't seen during this walk
// (i.e. it was deleted or moved outside the configured include directories). No-ops if the
// identifier or its doc has already vanished, rather than erroring — another prune could have
// raced it, or the metadata could already be gone.
fn prune_stale_file(db: &Database, id: FileId) -> Result<(), DirErrors> {
    let Some(doc_id) = db.get_doc_id_by_file_id(id)? else {
        return Ok(());
    };
    let Some(old_metadata) = db.get_metadata(doc_id)? else {
        return Ok(());
    };

    let old_file_name = normalize_file_name(
        old_metadata
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_ref(),
    );

    db.delete_document(id, &old_file_name)?;

    Ok(())
}

// Validates that the provided path exists and is a directory. Returns an error if the path does not exist or is not a directory.
fn validate_dir(dir: &Path) -> Result<(), DirErrors> {
    if !dir.exists() {
        return Err(DirErrors::DirectoryDoesntExist(dir.to_path_buf()));
    }

    if !dir.is_dir() {
        return Err(DirErrors::NotADirectory(dir.to_path_buf()));
    }

    Ok(())
}

// Classifies a file and dispatches it to the right processor. Only called for files that
// `index_state` reports as `New`.
fn index_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
    match classify(path)? {
        FileType::Text => process_text_file(path, db),
        file_type @ (FileType::Binary | FileType::Image) => {
            process_binary_and_image_file(path, db, file_type)
        }
    }
}

// Reindexes a file that `index_state` reports as `Modified` (content changed, renamed, or
// both), updating its existing doc instead of minting a new one.
fn reindex_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
    match classify(path)? {
        FileType::Text => process_and_reindex_text_file(path, db),
        file_type @ (FileType::Binary | FileType::Image) => {
            process_and_reindex_binary_and_images(path, db, file_type)
        }
    }
}

// Tells new, unchanged, and modified files apart. Primarily keys off the file's platform
// identifier (`util::file_id`, stable across renames/moves) rather than its path: looks up
// the doc last indexed under that identifier and compares its old path/mtime against the
// file's current path/mtime, so a rename alone (same content, new path) is classified as
// `Modified` just like a content change, instead of looking like a brand-new file. Falls back
// to path-only comparison (today's behavior, blind to renames) when the platform/filesystem
// can't provide an identifier — rare, but possible on some Windows volumes.
fn index_state(
    path: &Path,
    db: &Database,
    file_ids: &mut HashSet<IVec>,
) -> Result<IndexState, DirErrors> {
    let fs_metadata = std::fs::metadata(path)?;
    let current_modified = modified_secs(&fs_metadata);

    if let Some(id) = file_id(&fs_metadata) {
        file_ids.insert(IVec::from(id.to_bytes().to_vec()));
        return Ok(match db.get_doc_id_by_file_id(id)? {
            None => IndexState::New,
            Some(doc_id) => match db.get_metadata(doc_id)? {
                Some(old_metadata)
                    if old_metadata.path == path && old_metadata.modified >= current_modified =>
                {
                    IndexState::Unchanged
                }
                Some(_) => IndexState::Modified,
                None => IndexState::New,
            },
        });
    }

    Ok(match db.get_file_modified_time(path)? {
        None => IndexState::New,
        Some(recorded_modified) if current_modified > recorded_modified => IndexState::Modified,
        Some(_) => IndexState::Unchanged,
    })
}
