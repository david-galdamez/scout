use std::{ffi::OsStr, fs::File, io::Read, path::Path};

use content_inspector::inspect;

use crate::{
    database::FileType,
    indexer::{extension_map::EXTENSION_MAP, file_walker::DirErrors},
};

pub fn classify(path: &Path) -> Result<FileType, DirErrors> {
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        let extension = ext.to_lowercase();
        if let Some(file_type) = EXTENSION_MAP.get(extension.as_str()) {
            return Ok(*file_type);
        }
    }

    inspect_file(path)
}

fn inspect_file(path: &Path) -> Result<FileType, DirErrors> {
    let mut file = File::open(path)?;
    let mut buffer = [0; 8192];

    let bytes_read = file.read(&mut buffer)?;

    if inspect(&buffer[..bytes_read]).is_text() {
        Ok(FileType::Text)
    } else {
        Ok(FileType::Binary)
    }
}
