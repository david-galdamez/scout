use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::Result;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::Config,
    database::Database,
    tui::{ConfigUpdate, IndexingEvent},
};

mod config;
mod database;
mod indexer;
mod search;
mod tui;
mod util;

// How long a burst of filesystem events is allowed to stay quiet before it's treated as
// settled and folded into one reindex. Long enough that an editor's atomic save (which fires
// several raw events in quick succession) or a `git checkout`/build touching thousands of
// files collapses into a single reindex rather than many; short enough that a real edit is
// still reflected promptly.
const DEBOUNCE_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> Result<()> {
    let config = config::load_and_validate_config()?;

    let db_path = Config::default_db_path()?;
    let mut log = open_log(&db_path);
    let db = Database::new(&db_path)?;
    let thread_db = db.clone();
    let (tx, rx) = mpsc::channel::<IndexingEvent>();
    let (config_tx, config_rx) = crossbeam_channel::unbounded::<ConfigUpdate>();
    // `notify::recommended_watcher`'s Linux backend watches for `IN_OPEN` on every file, with
    // no way to turn it off — meaning our own `File::open` calls while indexing (reading every
    // text file to tokenize it) generate an `EventKind::Access(AccessKind::Open(_))` for every
    // file we just read. Neither `notify-debouncer-mini` nor `-full` filter these out (checked
    // both crates' source — they treat every raw event as a potential change), so left alone
    // this is a self-sustaining loop: reindex opens every file -> that "opens" every file ->
    // debounced into another reindex -> repeat forever. So `notify_tx` only ever carries paths
    // from events that survive the `EventKind::Access(_)` filter below, and debouncing is
    // hand-rolled instead of via a crate, since neither would let us filter first.
    let (notify_tx, notify_rx) = crossbeam_channel::unbounded::<notify::Result<PathBuf>>();
    let tui_config = config.clone();

    let mut watcher =
        notify::recommended_watcher(move |result: notify::Result<Event>| match result {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) => {
                for path in event.paths {
                    let _ = notify_tx.send(Ok(path));
                }
            }
            Err(e) => {
                let _ = notify_tx.send(Err(e));
            }
        })?;
    let mut watched_dirs: HashSet<PathBuf> = HashSet::new();
    let mut exclude = exclude_set(&config);
    watch_all(
        &mut watcher,
        &indexer::indexable_dirs(&config.indexing.include, &exclude),
        &mut watched_dirs,
        &mut log,
    );

    thread::spawn(move || {
        let mut config = config;
        // `watcher` is moved into this closure and kept alive for the thread's lifetime —
        // dropping it would stop watching entirely.
        reindex(&config, &thread_db, &tx);

        // `pending`/`deadline` implement the debounce: any qualifying event pushes the
        // deadline out another `DEBOUNCE_TIMEOUT`, so a burst of changes collapses into one
        // reindex once things go quiet, rather than reindexing per event. `never()` keeps the
        // deadline arm permanently unready while nothing is pending, instead of firing
        // immediately.
        let mut pending = false;
        let mut deadline = crossbeam_channel::never();

        loop {
            crossbeam_channel::select! {
                recv(config_rx) -> msg => {
                    let Ok(update) = msg else {
                        // The TUI (and with it, the `App` holding `config_tx`) has exited.
                        break;
                    };

                    config = update.config;
                    exclude = exclude_set(&config);
                    if update.index_now {
                        let new_dirs = indexer::indexable_dirs(&config.indexing.include, &exclude);
                        resync_watches(&mut watcher, &new_dirs, &mut watched_dirs, &mut log);
                        reindex(&config, &thread_db, &tx);
                    }
                }
                recv(notify_rx) -> msg => {
                    let Ok(result) = msg else {
                        // The watcher's own background thread has gone away.
                        break;
                    };

                    match result {
                        Ok(path) => {
                            log_line(&mut log, &format!("Reindex triggered by change at {}", path.display()));
                            watch_new_directory(&mut watcher, &path, &exclude, &mut watched_dirs, &mut log);
                            pending = true;
                            deadline = crossbeam_channel::after(DEBOUNCE_TIMEOUT);
                        }
                        Err(e) => log_line(&mut log, &format!("Filesystem watch error: {e}")),
                    }
                }
                recv(deadline) -> _ => {
                    if pending {
                        pending = false;
                        reindex(&config, &thread_db, &tx);
                    }
                    deadline = crossbeam_channel::never();
                }
            }
        }
    });

    tui::run(db, &rx, tui_config, config_tx)?;

    Ok(())
}

fn exclude_set(config: &Config) -> HashSet<String> {
    config.indexing.exclude.iter().map(String::from).collect()
}

// The TUI takes over the terminal's alternate screen for its whole run, so anything written to
// stderr during that time either doesn't show up at all or shows up mangled once the terminal
// is restored — useless for diagnosing things like "why did it just reindex". Diagnostics go to
// a plain log file next to the sled database instead (`<data dir>/scout/scout.log`, append
// mode). `Option<File>` rather than a hard failure: losing diagnostics because the log file
// couldn't be opened shouldn't take down indexing itself.
fn log_path(db_path: &Path) -> Option<PathBuf> {
    db_path.parent().map(|dir| dir.join("scout.log"))
}

fn open_log(db_path: &Path) -> Option<File> {
    let path = log_path(db_path)?;
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn log_line(log: &mut Option<File>, message: &str) {
    let Some(file) = log.as_mut() else {
        return;
    };
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string());
    let _ = writeln!(file, "{timestamp} {message}");
}

// Registers a non-recursive watch on every directory in `dirs` not already in `watched_dirs`,
// inserting each into `watched_dirs` on success. Non-recursive because `dirs` is already the
// full, flattened set of every directory `walk_dirs` would actually index (via
// `indexer::indexable_dirs`) — watching each individually, rather than recursively from the
// configured roots, is what keeps gitignored/excluded subtrees (`node_modules`, `target`, …)
// off the watch list entirely.
fn watch_all(
    watcher: &mut dyn Watcher,
    dirs: &[PathBuf],
    watched_dirs: &mut HashSet<PathBuf>,
    log: &mut Option<File>,
) {
    for dir in dirs {
        watch_one(watcher, dir, watched_dirs, log);
    }
}

fn watch_one(
    watcher: &mut dyn Watcher,
    dir: &Path,
    watched_dirs: &mut HashSet<PathBuf>,
    log: &mut Option<File>,
) {
    if watched_dirs.contains(dir) {
        return;
    }
    match watcher.watch(dir, RecursiveMode::NonRecursive) {
        Ok(()) => {
            watched_dirs.insert(dir.to_path_buf());
        }
        Err(e) => log_line(log, &format!("Failed to watch path {}: {e}", dir.display())),
    }
}

// Brings the watch set in line with a new `include`/`exclude` configuration: unwatches
// whatever's no longer indexable, watches whatever newly is. Replaces the previous WIP's
// approach of diffing against a stale `Config` snapshot — this always diffs against the
// live, actually-watched set.
fn resync_watches(
    watcher: &mut dyn Watcher,
    new_dirs: &[PathBuf],
    watched_dirs: &mut HashSet<PathBuf>,
    log: &mut Option<File>,
) {
    let new_dirs: HashSet<PathBuf> = new_dirs.iter().cloned().collect();

    let removed: Vec<PathBuf> = watched_dirs.difference(&new_dirs).cloned().collect();
    for dir in removed {
        if let Err(e) = watcher.unwatch(&dir) {
            log_line(
                log,
                &format!("Failed to unwatch path {}: {e}", dir.display()),
            );
        }
        watched_dirs.remove(&dir);
    }

    for dir in &new_dirs {
        watch_one(watcher, dir, watched_dirs, log);
    }
}

// Because watches are non-recursive, a brand-new subdirectory created inside an
// already-watched one needs its own watch registered, or everything under it goes unseen
// until the app restarts. Checked on every qualifying event path — the raw `EventKind` isn't
// threaded through past the `Access` filter in `main`, so this just checks the filesystem
// directly rather than trusting the event's kind. Only filters by the literal `exclude` name
// (not a full `.gitignore` check) — a
// brand-new directory that happens to match a gitignore pattern could get watched before its
// own `.gitignore` is known. That's an acceptable approximation: this only decides what gets
// *watched*, not what gets *indexed* — `walk_dirs` re-applies full filtering on every
// reindex, so over-watching costs a bit of extra filesystem-watch overhead, never incorrect
// index contents.
fn watch_new_directory(
    watcher: &mut dyn Watcher,
    path: &Path,
    exclude: &HashSet<String>,
    watched_dirs: &mut HashSet<PathBuf>,
    log: &mut Option<File>,
) {
    if !path.is_dir() || watched_dirs.contains(path) {
        return;
    }
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| exclude.contains(name))
    {
        return;
    }
    watch_one(watcher, path, watched_dirs, log);
}

fn reindex(config: &Config, db: &Database, tx: &mpsc::Sender<IndexingEvent>) {
    let _ = tx.send(IndexingEvent::Started);
    let errors = indexer::walk_dirs(&config.indexing.include, &exclude_set(config), db);
    let _ = tx.send(IndexingEvent::Finished { errors });
}
