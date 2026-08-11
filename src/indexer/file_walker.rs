use std::{
    collections::HashSet,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

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
    },
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
    #[error("File modified since it was indexed; reindexing isn't implemented yet")]
    PendingReindex,
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
                        match index_state(entry.path(), db) {
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

    errors
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
// `index_state` reports as `New` — `Modified` files are collected as `DirErrors::PendingReindex`
// instead, since reusing this would mint a second doc_id for the same path.
fn index_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
    match classify(path)? {
        FileType::Text => process_text_file(path, db),
        file_type @ (FileType::Binary | FileType::Image) => {
            process_binary_and_image_file(path, db, file_type)
        }
    }
}

// Reindexes a file that has been modified since it was last indexed. This function is currently not implemented and will return an error if called.
fn reindex_file(path: &Path, db: &Database) -> Result<(), DirErrors> {
    match classify(path)? {
        FileType::Text => process_and_reindex_text_file(path, db),
        file_type @ (FileType::Binary | FileType::Image) => {
            process_and_reindex_binary_and_images(path, db, file_type)
        }
    }
}

// Compares the file's current mtime against the mtime recorded in `Metadata` the last time
// it was indexed (looked up via the `paths` tree) to tell new, unchanged, and modified files
// apart.
fn index_state(path: &Path, db: &Database) -> Result<IndexState, DirErrors> {
    let current_modified = std::fs::metadata(path)?.mtime().cast_unsigned();

    Ok(match db.get_file_modified_time(path)? {
        None => IndexState::New,
        Some(recorded_modified) if current_modified > recorded_modified => IndexState::Modified,
        Some(_) => IndexState::Unchanged,
    })
}
