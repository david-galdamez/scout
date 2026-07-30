use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    path::Path,
};

use content_inspector::inspect;
use thiserror::Error;

use crate::{database::FileType, indexer::extension_map::EXTENSION_MAP};

#[derive(Debug, Error)]
enum ClassifierError {
    #[error("I/O error: {0}")]
    FilerError(#[from] io::Error),
}

pub fn classify(path: &Path) -> Result<FileType, ClassifierError> {
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        let extension = ext.to_lowercase();
        if let Some(file_type) = EXTENSION_MAP.get(extension.as_str()) {
            return Ok(*file_type);
        }
    }

    inspect_file(path)
}

fn inspect_file(path: &Path) -> Result<FileType, ClassifierError> {
    let file = File::open(path)?;
    let mut buffer = [0; 8192];

    let mut handler = file.take(8192);

    handler.read(&mut buffer)?;
    let content_type = inspect(&buffer);

    if content_type.is_text() {
        Ok(FileType::Text)
    } else {
        Ok(FileType::Binary)
    }
}
