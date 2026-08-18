use std::path::PathBuf;

use crate::{config::Config, indexer::DirErrors};

pub enum IndexingEvent {
    Started,
    Finished { errors: Vec<(PathBuf, DirErrors)> },
}

// Sent from the TUI to the background indexing thread when the user saves changes on the
// config screen. `index_now` is false when the save didn't actually change `include`/`exclude`
// (nothing to walk again for), so the background thread can skip straight back to sleeping out
// the rest of its interval instead of doing a needless walk.
pub struct ConfigUpdate {
    pub config: Config,
    pub index_now: bool,
}
