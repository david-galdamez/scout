use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{LockResult, Mutex, PoisonError},
};

use ignore::{DirEntry, WalkBuilder, WalkState};
use sled::IVec;
use thiserror::Error;

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

// Recovers a `Mutex`'s value even if some other thread panicked while holding it, rather than
// propagating that panic here. None of the critical sections in this file do anything that
// this crate's lint set would let panic (no raw arithmetic, no unwraps, no indexing) — a
// poisoned lock could only come from something outside our control, and losing the whole
// walk's progress over that would be worse than continuing with the guarded data as-is.
fn recover<T>(result: LockResult<T>) -> T {
    result.unwrap_or_else(PoisonError::into_inner)
}

// Leaves one core free for the TUI's own thread rather than handing every core to the walk
// (`WalkBuilder`'s own default when `.threads(0)`). This app runs the walk on a 5-minute
// timer in the background while the user is actively typing/searching, so saturating every
// core makes the OS scheduler starve the single UI thread of time slices — felt as the whole
// app freezing mid-keystroke, even though typing itself never touches the database. Floors at
// 1 so a single-core machine still gets a (shared) worker thread instead of none.
fn worker_thread_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .saturating_sub(1)
        .max(1)
}

// Configures a `WalkBuilder` rooted at `dir` with this app's filtering rules — shared by
// `walk_dirs` (which walks it in parallel, indexing every file) and `indexable_dirs` (which
// walks it sequentially, collecting only directories, to build the filesystem-watch list) so
// the two can never drift apart on what counts as "in scope."
fn configure_walker(dir: &Path, exclude: &HashSet<String>) -> WalkBuilder {
    let exclude = exclude.clone();
    let mut builder = WalkBuilder::new(dir);
    builder
        .threads(worker_thread_count())
        .filter_entry(move |entry| !exclude.contains(entry.file_name().to_str().unwrap_or("")))
        // Only `.gitignore`/`.ignore` files *inside* the walked tree apply — one sitting
        // above `dir` (e.g. in the user's home directory) shouldn't reach in and hide
        // files the user explicitly configured to be indexed.
        .parents(false)
        // The previous engine (`walkdir`) had no notion of "hidden", so dotfiles were
        // indexed unless excluded by name — keep that behavior rather than silently
        // dropping every dotfile now that `ignore`'s default is to skip them.
        .hidden(false)
        // Same reasoning as `parents`: the user's global git excludesFile is unrelated to
        // this app and lives outside the walked tree, so don't let it affect what's
        // indexed.
        .git_global(false);
    // `.gitignore` respect (`git_ignore`, on by default) only activates inside an actual
    // git repository — `require_git` (also on by default, left as-is here) means a
    // `.gitignore`-named file with no `.git` anywhere at/above it is not treated
    // specially. That's fine for the common case this is meant to solve: `dir` itself
    // (e.g. `~/Dev`) usually isn't a repo, but the individual projects nested under it
    // are, and each one's own `.gitignore` still applies once the walk descends into it.
    builder
}

// Walks through the provided directories, classifies files, and processes them accordingly.
// Returns a vector of errors encountered during the walk.
//
// Each `dir` is walked with `ignore::WalkBuilder`/`WalkParallel` (the engine behind
// ripgrep/fd) rather than a plain single-threaded walk: it spreads traversal, classification,
// and indexing across a thread pool sized to the machine's core count, and it's
// `.gitignore`/`.ignore`-aware, so a project's own ignore rules prune dependency/build
// directories automatically instead of relying solely on the hand-maintained `exclude` list.
// `exclude` is layered on top via `filter_entry` as an explicit, additional prune — config
// semantics for it are unchanged from before.
pub fn walk_dirs(
    dirs: &Vec<PathBuf>,
    exclude: &HashSet<String>,
    db: &Database,
) -> Vec<(PathBuf, DirErrors)> {
    let errors: Mutex<Vec<(PathBuf, DirErrors)>> = Mutex::new(Vec::new());
    let visited_file_ids: Mutex<HashSet<IVec>> = Mutex::new(HashSet::new());

    for dir in dirs {
        if let Err(e) = validate_dir(dir) {
            recover(errors.lock()).push((dir.clone(), e));
            continue;
        }

        let builder = configure_walker(dir, exclude);

        builder.build_parallel().run(|| {
            Box::new(|entry_result: Result<DirEntry, ignore::Error>| {
                match entry_result {
                    Ok(entry) => {
                        if entry.file_type().is_some_and(|ft| ft.is_file()) {
                            visit_file(entry.path(), db, &errors, &visited_file_ids);
                        }
                    }
                    Err(err) => record_walk_error(&errors, err),
                }
                WalkState::Continue
            })
        });
    }

    let file_ids = match db.get_all_file_ids() {
        Ok(ids) => ids,
        Err(e) => {
            recover(errors.lock()).push((PathBuf::new(), DirErrors::DatabaseError(e)));
            return recover(errors.into_inner());
        }
    };

    let visited_file_ids = recover(visited_file_ids.into_inner());
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
            recover(errors.lock()).push((PathBuf::new(), e));
        }
    }

    recover(errors.into_inner())
}

// Every directory that would survive `walk_dirs`'s filtering (the configured `exclude` set,
// plus `.gitignore`/`.ignore` rules), including each root in `dirs` itself. Used to build a
// filesystem-watch list that mirrors what's actually indexed, rather than watching
// dependency/build directories that would never be indexed in the first place. A plain
// sequential walk is enough here — it's directories only (no file classification/indexing
// work), and only run at startup or when `include`/`exclude` changes, not per file.
pub fn indexable_dirs(dirs: &[PathBuf], exclude: &HashSet<String>) -> Vec<PathBuf> {
    let mut result = Vec::new();

    for dir in dirs {
        if validate_dir(dir).is_err() {
            continue;
        }

        for entry in configure_walker(dir, exclude).build().flatten() {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                result.push(entry.path().to_path_buf());
            }
        }
    }

    result
}

// Classifies and indexes/reindexes a single file, recording any failure into `errors` rather
// than stopping the walk. Called concurrently, once per file, from every worker thread.
fn visit_file(
    path: &Path,
    db: &Database,
    errors: &Mutex<Vec<(PathBuf, DirErrors)>>,
    visited_file_ids: &Mutex<HashSet<IVec>>,
) {
    match index_state(path, db, visited_file_ids) {
        Ok(IndexState::Unchanged) => {}
        Ok(IndexState::New) => {
            if let Err(e) = index_file(path, db) {
                recover(errors.lock()).push((path.to_path_buf(), e));
            }
        }
        Ok(IndexState::Modified) => {
            if let Err(e) = reindex_file(path, db) {
                recover(errors.lock()).push((path.to_path_buf(), e));
            }
        }
        Err(e) => recover(errors.lock()).push((path.to_path_buf(), e)),
    }
}

// Converts a directory/file-level error surfaced by the walk itself (permission denial,
// symlink loop, generic I/O failure) into a `DirErrors` and records it.
fn record_walk_error(errors: &Mutex<Vec<(PathBuf, DirErrors)>>, error: ignore::Error) {
    if let Some(ancestor) = loop_ancestor(&error) {
        let ancestor = ancestor.to_path_buf();
        recover(errors.lock()).push((ancestor, DirErrors::SymlinkLoop));
        return;
    }

    let path = error_path(&error).map_or_else(PathBuf::new, Path::to_path_buf);
    // Computed before `into_io_error()` consumes `error`, for the fallback branch below.
    let message = error.to_string();

    match error.into_io_error() {
        Some(inner) if inner.kind() == std::io::ErrorKind::PermissionDenied => {
            recover(errors.lock()).push((path, DirErrors::PermissionDenied));
        }
        Some(inner) => recover(errors.lock()).push((path, DirErrors::IoError(inner))),
        None => {
            // A non-I/O `ignore::Error` (e.g. a malformed `.gitignore` line) — nothing in
            // `DirErrors` models this specifically, so carry its message through as a
            // synthetic I/O error rather than adding a variant for a case this app never
            // triggers deliberately (we don't configure globs/type overrides).
            recover(errors.lock()).push((path, DirErrors::IoError(std::io::Error::other(message))));
        }
    }
}

// `ignore::Error`'s path/loop-ancestor info can be nested a few layers deep (e.g. `WithDepth`
// wrapping the actual `WithPath`/`Loop`) — these walk down through the wrapper variants to
// find it, mirroring how `walkdir::Error` exposed the same information through dedicated
// `.path()`/`.loop_ancestor()` methods.
fn error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path.as_path()),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            error_path(err)
        }
        _ => None,
    }
}

fn loop_ancestor(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::Loop { ancestor, .. } => Some(ancestor.as_path()),
        ignore::Error::WithPath { err, .. }
        | ignore::Error::WithLineNumber { err, .. }
        | ignore::Error::WithDepth { err, .. } => loop_ancestor(err),
        _ => None,
    }
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
    visited_file_ids: &Mutex<HashSet<IVec>>,
) -> Result<IndexState, DirErrors> {
    let fs_metadata = std::fs::metadata(path)?;
    let current_modified = modified_secs(&fs_metadata);

    if let Some(id) = file_id(&fs_metadata) {
        recover(visited_file_ids.lock()).insert(IVec::from(id.to_bytes().to_vec()));
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::database::Database;

    fn open_db() -> (TempDir, Database) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let db = Database::new(dir.path()).expect("failed to open database");
        (dir, db)
    }

    #[test]
    fn respects_gitignore_inside_the_walked_tree() {
        let (_db_dir, db) = open_db();
        let tree = tempfile::tempdir().expect("failed to create temp dir");

        // `ignore` only honors `.gitignore` inside an actual git repo (`require_git` defaults
        // to `true`) — a bare `.gitignore` with no `.git` marker anywhere at/above it is not
        // treated specially. A `.git` dir (even an empty one — the walker only checks for its
        // presence, not that it's a real repo) is what makes this test representative of the
        // real case: `dir` itself (e.g. `~/Dev`) usually isn't a repo, but the projects nested
        // under it are.
        let project = tree.path().join("project");
        std::fs::create_dir(&project).expect("failed to create dir");
        std::fs::create_dir(project.join(".git")).expect("failed to create dir");
        std::fs::write(project.join(".gitignore"), "ignored/\n")
            .expect("failed to write .gitignore");
        std::fs::create_dir(project.join("ignored")).expect("failed to create dir");
        std::fs::write(project.join("ignored").join("skip.txt"), b"skip")
            .expect("failed to write file");
        std::fs::write(project.join("keep.txt"), b"keep").expect("failed to write file");

        let errors = walk_dirs(&vec![tree.path().to_path_buf()], &HashSet::new(), &db);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        assert!(
            db.get_doc_id_by_path(&project.join("keep.txt"))
                .expect("get_doc_id_by_path failed")
                .is_some()
        );
        assert!(
            db.get_doc_id_by_path(&project.join("ignored").join("skip.txt"))
                .expect("get_doc_id_by_path failed")
                .is_none()
        );
    }

    #[test]
    fn exclude_set_still_prunes_directories_without_gitignore() {
        let (_db_dir, db) = open_db();
        let tree = tempfile::tempdir().expect("failed to create temp dir");

        std::fs::create_dir(tree.path().join("node_modules")).expect("failed to create dir");
        std::fs::write(
            tree.path().join("node_modules").join("dep.js"),
            b"module.exports = {}",
        )
        .expect("failed to write file");
        std::fs::write(tree.path().join("keep.txt"), b"keep").expect("failed to write file");

        let exclude = HashSet::from(["node_modules".to_string()]);
        let errors = walk_dirs(&vec![tree.path().to_path_buf()], &exclude, &db);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        assert!(
            db.get_doc_id_by_path(&tree.path().join("keep.txt"))
                .expect("get_doc_id_by_path failed")
                .is_some()
        );
        assert!(
            db.get_doc_id_by_path(&tree.path().join("node_modules").join("dep.js"))
                .expect("get_doc_id_by_path failed")
                .is_none()
        );
    }

    #[test]
    fn concurrent_walk_indexes_every_file_exactly_once_and_is_idempotent() {
        const FILE_COUNT: usize = 64;

        let (_db_dir, db) = open_db();
        let tree = tempfile::tempdir().expect("failed to create temp dir");

        for i in 0..FILE_COUNT {
            let sub = tree.path().join(format!("dir_{i}"));
            std::fs::create_dir(&sub).expect("failed to create dir");
            std::fs::write(sub.join("file.txt"), format!("content {i}"))
                .expect("failed to write file");
        }

        let dirs = vec![tree.path().to_path_buf()];
        let errors = walk_dirs(&dirs, &HashSet::new(), &db);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");

        let file_ids = db.get_all_file_ids().expect("get_all_file_ids failed");
        assert_eq!(file_ids.len(), FILE_COUNT);

        // A second, unchanged walk must not create duplicate docs or errors — every file
        // should resolve to `IndexState::Unchanged` regardless of which worker thread reaches
        // it first.
        let errors = walk_dirs(&dirs, &HashSet::new(), &db);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let file_ids_after = db.get_all_file_ids().expect("get_all_file_ids failed");
        assert_eq!(file_ids_after.len(), FILE_COUNT);
    }

    #[test]
    fn indexable_dirs_matches_what_walk_dirs_would_index() {
        let tree = tempfile::tempdir().expect("failed to create temp dir");

        // Same shape as `respects_gitignore_inside_the_walked_tree`: a nested "project" with
        // its own `.git` marker and `.gitignore`, plus a literal `exclude` entry alongside it —
        // `indexable_dirs` should reflect both kinds of filtering.
        let project = tree.path().join("project");
        std::fs::create_dir(&project).expect("failed to create dir");
        std::fs::create_dir(project.join(".git")).expect("failed to create dir");
        std::fs::write(project.join(".gitignore"), "ignored/\n")
            .expect("failed to write .gitignore");
        std::fs::create_dir(project.join("ignored")).expect("failed to create dir");
        std::fs::create_dir(project.join("kept")).expect("failed to create dir");
        std::fs::create_dir(tree.path().join("node_modules")).expect("failed to create dir");

        let exclude = HashSet::from(["node_modules".to_string()]);
        let dirs = indexable_dirs(&[tree.path().to_path_buf()], &exclude);

        assert!(dirs.contains(&tree.path().to_path_buf()));
        assert!(dirs.contains(&project));
        assert!(dirs.contains(&project.join("kept")));
        assert!(!dirs.contains(&project.join("ignored")));
        assert!(!dirs.contains(&tree.path().join("node_modules")));
    }
}
