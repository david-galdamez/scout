use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use thiserror::Error;
use walkdir::WalkDir;

use crate::{
    database::{Database, FileType},
    indexer::{
        classifier::classify,
        processors::{process_binary_and_image_file, process_text_file},
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
                        match classify(entry.path()) {
                            Ok(FileType::Text) => {
                                if let Err(e) = process_text_file(entry.path(), db) {
                                    errors.push((entry.path().to_path_buf(), e));
                                }
                            }
                            Ok(file_type @ (FileType::Binary | FileType::Image)) => {
                                if let Err(e) =
                                    process_binary_and_image_file(entry.path(), db, file_type)
                                {
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
