use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use thiserror::Error;
use walkdir::WalkDir;

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
}

pub fn walk_dirs(
    dirs: Vec<PathBuf>,
    exclude: HashSet<String>,
) -> Result<Vec<(PathBuf, DirErrors)>, DirErrors> {
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
                        println!("{}", entry.path().display());
                    }
                }
                Err(e) => {
                    let path = e
                        .path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();

                    let loop_path = e.loop_ancestor().unwrap_or(Path::new("")).to_path_buf();

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

    Ok(errors)
}

fn validate_dir(dir: &Path) -> Result<(), DirErrors> {
    if !dir.exists() {
        return Err(DirErrors::DirectoryDoesntExist(dir.to_path_buf()));
    }

    if !dir.is_dir() {
        return Err(DirErrors::NotADirectory(dir.to_path_buf()));
    }

    Ok(())
}
