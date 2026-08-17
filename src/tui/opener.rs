use std::{error::Error as _, path::Path};

use opener::{OpenError, reveal};

pub fn reveal_document(path: &Path) -> Result<(), OpenError> {
    reveal(path)
}

// `OpenError`'s own `Display` is unhelpfully generic (`OpenError::Io` always prints just "IO
// error", with neither the path nor the OS's actual reason) — the real detail lives in
// `source()`, which `Display` never surfaces. Build a message with both `path` and that source
// so a failure like a moved/deleted directory reads as e.g. "Couldn't open /old/path: No such
// file or directory (os error 2)" instead of just "IO error".
pub fn describe_open_error(path: &Path, error: &OpenError) -> String {
    error.source().map_or_else(
        || format!("Couldn't open {}: {error}", path.display()),
        |source| format!("Couldn't open {}: {source}", path.display()),
    )
}
