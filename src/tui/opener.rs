use std::path::Path;

use opener::{OpenError, reveal};

pub fn reveal_document(path: &Path) -> Result<(), OpenError> {
    reveal(path)
}
