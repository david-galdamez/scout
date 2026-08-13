use std::path::PathBuf;

use crate::indexer::DirErrors;

pub enum IndexingEvent {
    Started,
    Finished { errors: Vec<(PathBuf, DirErrors)> },
}
