# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Scout is a local file search engine written in Rust. It recursively indexes files from
user-configured directories, tokenizes text content, and stores an inverted index in an
embedded `sled` database, searchable via title-prefix match, file-name-token match, and
BM-25 ranking. A `ratatui` TUI is the frontend, backed by a periodic indexing pass that runs
on its own thread so the UI is never blocked on a walk.

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Test all: `cargo test`
- Test a single test: `cargo test <test_name>` (e.g. `cargo test strips_accents`)
- Lint: `cargo clippy`
- Format: `cargo fmt`

## Architecture

Pipeline, wired together in `src/main.rs`:

```
config::load_and_validate_config()  ->  Config
        |
        v
Database::new(db_path)              ->  sled-backed Database  ---clone()---> background thread
        |                                                              |
        v                                                              v
tui::run(db, index_rx,                                    loop { recv_timeout(deadline) ->
         config, config_tx)                                       ConfigUpdate  -> maybe reindex now
   (blocks until the user quits)                                  Timeout       -> reindex
                                                                    Disconnected  -> break }
        ^                                                               |
        | ConfigUpdate (on save, from the config screen)                v
        +--------------------------------------------------  indexer::walk_dirs(include, exclude, &db)
                                                            |
                                                            v
                                                for each file: index_state (new/unchanged/modified)
                                                  new      -> classify -> process_text_file / process_binary_and_image_file -> db.index_document / db.index_binary_and_image
                                                  unchanged -> skipped
                                                  modified  -> classify -> process_and_reindex_text_file / process_and_reindex_binary_and_images -> db.reindex_text_document / db.reindex_binary_and_image
                                                            |
                                                            v
                                                after the walk: any indexed file_id not seen during it -> prune_stale_file -> db.delete_document
                                                            |
                                                            v
                                        IndexingEvent::Started / Finished { errors } sent back to the TUI

search::Searcher::search(query)     ->  title-prefix match, else BM-25 over term tokens + file-name tokens
                                          (called directly from the TUI's own `Database` clone on every Enter)
```

### `config`
`src/config/load.rs` defines `Config { indexing: Indexing { include, exclude } }`, loaded
from `~/.scout.toml` (created with defaults on first run if missing, then validated).
Defaults index `~/Documents` and exclude common noise dirs (`node_modules`, `.git`, `target`,
etc.). The sled database itself lives under the OS data dir (`dirs::data_dir()/scout/index.db`),
separate from the config file.

### `database` (`src/database/`)
Wraps a `sled::Db` with nine trees, defined in `schemas.rs`:
- `metadata`: doc_id (u64 BE bytes) -> `Metadata` (path, size, mtime, `FileType`, doc_length)
- `paths`: path string -> doc_id (u64 BE bytes); a secondary lookup kept in sync on every
  write, used as the fallback identifier (see `file_ids` below) and by
  `get_file_modified_time`/`get_doc_id_by_path`
- `file_ids`: `FileId` bytes (`util::FileId` — device + inode on Unix, volume serial number +
  file index on Windows, via `util::file_id`) -> doc_id (u64 BE bytes); the *primary* way an
  existing doc is found again during a walk, because unlike `paths` it survives a rename
- `file_names`: normalized file name -> `Vec<doc_id>` (multiple docs can share a file name)
- `terms`: term -> `Vec<TermFrequency>` (doc_id + frequency), i.e. the inverted index over
  text content, populated only for `FileType::Text`
- `name_terms`: file-name token (split on `_` too, via `tokenize_file_name`) -> `Vec<doc_id>`;
  populated for every file regardless of type, so binaries/images are findable by name
- `document_terms` / `document_name_terms`: doc_id -> `Vec<String>`, the *reverse* of `terms`/
  `name_terms` — the set of content/name tokens a given doc is currently indexed under. Exists
  solely so reindexing can look up and remove a doc's stale postings from `terms`/`name_terms`
  without scanning the whole vocabulary.
- `stats`: aggregate counters (`n_text_docs`, `n_terms`, `n_binary_docs`, `n_image_docs`) used
  for BM-25 normalization via `get_stats` -> `Stats { total_text_docs, avg_total_terms }`

`Database::index_document` and `Database::index_binary_and_image` (`repository.rs`) are the
fresh-index write paths: each generates a doc id via sled's atomic counter, then writes
metadata/paths/(file_ids)/file_names/(terms)/name_terms/document_(name_)terms/stats inside
**one sled transaction** across all the trees it touches, so a document is indexed atomically
or not at all. Both take an `Option<FileId>` (`None` when the platform/filesystem couldn't
provide one) and write it to `file_ids` only when `Some`. `index_document` additionally writes
to `terms` and bumps `n_text_docs`/`n_terms`; `index_binary_and_image` skips `terms` and bumps
`n_binary_docs`/`n_image_docs` by `FileType`.

`Database::reindex_text_document` / `reindex_binary_and_image` are the update path for a doc
that already exists. Unlike the fresh-index functions, they take an already-resolved `doc_id`
as their first argument instead of looking one up by path — the caller (see `processors.rs`
below) is expected to have resolved it via `file_ids` (surviving a rename), so these functions
never assume `metadata.path` is unchanged. They read the doc's previous `Metadata` internally
(for the old `doc_length`, used for the `stats` delta, and the old `path`, to know whether
`paths` needs its stale entry removed and a new one inserted), read old postings from
`document_terms`/`document_name_terms`, remove `doc_id` from the stale `terms`/`name_terms`/
`file_names` entries (helpers `remove_stale_postings`/`remove_name_postings`, deleting a
postings list entirely once empty), adjust `stats` by the doc_length delta (`decrease_stat`
then `bump_stats`, rather than just adding again), and re-insert fresh postings
(`insert_fresh_postings`/`insert_name_postings` — shared with the fresh-index path). Note they
don't touch `file_ids`: a file's platform identifier doesn't change across a rename or edit,
so the tree only ever needs writing once, at first index. `DatabaseError` wraps `sled::Error`
and `serde_json::Error`; note the two-layer transaction error handling —
`ConflictableTransactionError` inside the closure vs. `TransactionError` once it escapes
`.transaction(...)`.

`Database::delete_document(file_id, file_name)` is the removal path for a doc whose file no
longer exists on disk. It resolves `doc_id` via `file_ids` (erroring with
`DatabaseError::CollectionNotFound` if the id or its `Metadata` is missing), reads its old
`document_terms`/`document_name_terms` for cleanup, then in one transaction removes it from
`metadata`, `document_terms`, `document_name_terms`, `paths`, and `file_ids`, strips its
postings from `terms`/`name_terms`/`file_names` (`remove_stale_postings`, shared with the
reindex path), and decrements the right `stats` counter by `FileType` (`n_text_docs`/`n_terms`
for `Text`, `n_binary_docs`/`n_image_docs` for `Binary`/`Image`). `Database::get_all_file_ids`
returns every key currently in the `file_ids` tree, as raw `IVec`s, for the walker to diff
against what it saw.

### `indexer` (`src/indexer/`)
- `file_walker.rs`: `walk_dirs` walks each configured include directory with
  `ignore::WalkBuilder`/`WalkParallel` (the engine behind ripgrep/fd) rather than a
  single-threaded walk — it spreads traversal, classification, and indexing across a thread
  pool sized to the machine's core count (`WalkBuilder`'s own default, roughly
  `available_parallelism().min(12)`), and it's `.gitignore`/`.ignore`-aware, so a project's own
  ignore rules prune dependency/build directories automatically. `.gitignore` respect only
  activates inside an actual git repo (`require_git`, on by default, left untouched) — a
  `.gitignore`-named file with no `.git` anywhere at/above it is not treated specially, which
  is fine for the common case: the configured include directory itself (e.g. `~/Dev`) usually
  isn't a repo, but the individual projects nested under it are, and each one's own
  `.gitignore` applies once the walk descends into it. `.parents(false)` and `.git_global(false)`
  are set explicitly so only ignore files *inside* the walked tree apply — not a `.gitignore`
  sitting above the include directory, nor the user's global git excludesFile, either of which
  would otherwise reach in and hide files the user explicitly configured to be indexed.
  `.hidden(false)` is set explicitly too, since `ignore`'s default is to skip dotfiles and the
  previous engine (`walkdir`) had no such behavior — dotfiles are still indexed unless excluded
  by name, same as before. The configured `exclude: &HashSet<String>` set is layered on top via
  `filter_entry` as an additional, explicit prune (unchanged config semantics from before this
  rewrite).

  `errors: Mutex<Vec<(PathBuf, DirErrors)>>` and `visited_file_ids: Mutex<HashSet<IVec>>` are
  shared across every worker thread the walk spawns (`ignore::WalkParallel::run` uses
  `std::thread::scope` internally, so plain `&Mutex<_>` references work — no `Arc` needed, since
  the call is synchronous and every worker has joined by the time `run` returns). `Database` is
  passed by `&Database` into the per-thread visitor closures for the same reason; it's already
  cheap to clone/share across threads elsewhere in this codebase (`main.rs`'s background
  indexing thread and the TUI both hold their own clone). A small `recover` helper unwraps a
  `Mutex`'s value even from a poisoned lock (`PoisonError::into_inner`) rather than panicking,
  since nothing in these critical sections does anything this crate's lint set would let panic.
  `ignore::Error`'s path/loop-ancestor info can be nested behind `WithPath`/`WithDepth`/
  `WithLineNumber` wrapper variants — `error_path`/`loop_ancestor` walk down through them to
  find it, replacing the dedicated `.path()`/`.loop_ancestor()` methods `walkdir::Error` used to
  offer directly.

  Before indexing, `index_state` classifies each file as `New`, `Unchanged`, or `Modified`,
  primarily by computing the file's `util::file_id` and looking it up via
  `Database::get_doc_id_by_file_id`: no match is `New`; a match is `Unchanged` only if both
  the stored path and mtime match the file's current path/mtime, otherwise `Modified` — so a
  bare rename (same content, new path) is classified the same way as a content edit, rather
  than looking like a brand-new file. Falls back to the old path-only comparison (via
  `get_file_modified_time`, blind to renames) when the platform can't provide a `file_id`.
  `New` files go through `index_file` (classify -> process_*), and `Modified` files go through
  `reindex_file` (classify -> process_and_reindex_*). `index_state` also records every file's
  `FileId` (as raw bytes) into the shared `visited_file_ids` set, taking the lock only for that
  one insert rather than for the sled reads around it, to keep worker threads off each other's
  toes as much as possible. Once every configured directory has been walked, `walk_dirs` diffs
  that set against `Database::get_all_file_ids` (via `FileId::from_bytes`, which returns `None`
  — skipped rather than erroring — for a key of the wrong length) and calls `prune_stale_file`
  for every indexed `file_id` that wasn't visited: it looks up the doc's old `Metadata` to
  normalize its file name, then calls `db.delete_document`, no-oping rather than erroring if the
  id or doc has already vanished (e.g. a race with another prune). This is how deletes and
  moves-out-of-scope get reflected in the index — a file whose `file_id` is never seen again
  during a walk is treated as gone.
- `classifier.rs` / `extension_map.rs`: classifies a path as `Text`/`Image`/`Binary`, first
  by extension lookup (`EXTENSION_MAP`), falling back to content sniffing via
  `content_inspector::inspect` on the first 8KB when the extension is unknown.
- `processors.rs`: `process_text_file`/`process_and_reindex_text_file` read a text file
  line-by-line, tokenize it, build metadata via `std::fs::Metadata` + `util::modified_secs`
  (cross-platform — no `MetadataExt` outside `util::file_id`), count term frequencies, and
  call `db.index_document`/`db.reindex_text_document`. The reindex path resolves `doc_id` via
  the shared `resolve_doc_id` helper (`file_id` lookup, falling back to `get_doc_id_by_path`
  for docs that predate the `file_ids` tree or a platform without one), then reads the doc's
  old `Metadata` to normalize its old file name for postings cleanup — passing an
  *unnormalized* name here would silently no-op the cleanup, since `file_names`/`name_terms`
  keys are always normalized. `process_binary_and_image_file`/
  `process_and_reindex_binary_and_images` build metadata with `doc_length: 0` and call
  `db.index_binary_and_image`/`db.reindex_binary_and_image` the same way, so `Image`/`Binary`
  files are indexed by file name/path only, with no content extraction.
- `tokenizer.rs`: `tokenizer` lowercases, strips accents (`unicode-normalization` NFD +
  combining-mark filtering), splits on non-alphanumeric (except `_`), and filters a combined
  Spanish/English stopword list, for indexing file *content*. `normalize_file_name` /
  `tokenize_file_name` do the equivalent for file *names*, additionally splitting on `_` so
  snake_case names produce individual searchable tokens. Has unit tests covering these
  behaviors — check these when changing tokenization rules.

### `search` (`src/search/`)
`Searcher::search` (`searcher.rs`) tries three tiers in order, returning as soon as one
matches:
1. Title-prefix match: `Database::get_prefix_files` scans the `file_names` tree by prefix on
   the normalized query, unranked.
2. BM-25 + file-name-token fallback (`search_files`), when no prefix match is found:
   `tokenizer(query)` terms are looked up in `terms` and scored with the standard BM-25
   formula (`K1 = 1.5`, `B = 0.75`) using `Stats` from `get_stats`; separately,
   `tokenize_file_name(query)` terms are looked up in `name_terms` via `get_name_docs` — this
   is what surfaces `Binary`/`Image` files (which have no `terms` entries) and any text file
   whose name matches but whose content doesn't. BM-25-scored results are sorted by score
   descending; name-token matches are appended after, unranked.

### `tui` (`src/tui/`)
`main.rs` opens `Database` once, clones it for a `std::thread::spawn`ed loop, and creates two
channels: `mpsc::Sender<IndexingEvent>` (background -> TUI, `Started` / `Finished { errors }`,
defined in `index.rs`) and `mpsc::Sender<ConfigUpdate>` (TUI -> background, `{ config,
index_now }`, sent when the user saves the config screen). The background loop tracks a
`deadline` (`Instant`, starting already-elapsed so the first walk runs immediately at startup)
and blocks on `config_rx.recv_timeout(deadline - now)`: a `Timeout` runs `reindex` and pushes
the deadline out another `REINDEX_INTERVAL` (5 minutes); an `Ok(update)` swaps in the new
`Config` and only reindexes immediately if `update.index_now` is true (i.e. `include`/`exclude`
actually changed), still resetting the deadline either way; `Disconnected` (the TUI, and with
it `App`'s `config_tx`, exited) breaks the loop. `main` then calls
`tui::run(db, &rx, tui_config, config_tx)` with the original `Database` handle and a clone of
`Config` on the main thread. The background thread is never joined — `Database`/`Searcher`
reads and writes are safe to interleave across threads (sled transactions), so the TUI can
search while a walk is still in progress; process exit just drops the thread, which is fine
since sled writes are crash-safe.

- `app.rs`: `App` owns the `Database` handle, `query`/`results`/`selected` (for the results
  list), the screen/mode state machine, the last-saved `Config` (plus `config_tx` to notify the
  background thread on save), `index_status`/`errors` (populated from `IndexingEvent`s), and
  `results_area`/`results_list_state` (the results `List`'s last-rendered rect and
  `ListState`, kept so a mouse click's screen coordinates can be translated back into a result
  index via `result_index_at`). `Screen` is `Home` (landing page) / `Results` (query + list) /
  `Config` (manage `include`/`exclude`) / `Errors` (per-file errors from the last walk) / `Exit`
  (a confirmation overlay drawn on top of whichever other screen was active —
  `App::request_exit` snapshots the current screen into `previous_screen` so cancelling returns
  to the right place, and resets `exit_choice` to `ExitChoice::No` every time the popup opens so
  an accidental Enter can't quit; `open_config`/`open_errors` snapshot `previous_screen` the
  same way). `Action` (`Searching`/`Navigating`, toggled with Tab on the `Results` screen) tracks
  whether the query box or the results list has focus. `select_next`/`select_previous` wrap
  around the results list using `checked_add`/`checked_sub` (never raw `+`/`-`, since the crate
  denies `arithmetic_side_effects`); `set_results` replaces the list and resets `selected` to 0,
  since the old index may not make sense against a new result set. `on_indexing_event` updates
  `index_status` and, on `Finished`, replaces `errors` wholesale (not accumulated — each walk's
  errors supersede the last) and resets `errors_selected`.

  `ConfigDraft` (also in `app.rs`) is a working copy of `Indexing`, created by `open_config`
  from `App.config` and edited in place — nothing is persisted or sent to the background thread
  until `save_config`. It tracks which of the two lists (`ConfigList::Include`/`Exclude`) is
  focused and whether that focus is plain navigation or `AddInput` (typing a new/edited entry,
  `editing_index: Some` distinguishing an in-place edit from an append). While typing an
  `Include` entry, `update_suggestions` recomputes filesystem-backed autocomplete
  `suggestions` on every keystroke (`std::fs::read_dir` on the directory implied by the input's
  text up to its last `/`, filtered by what follows it, `~`-expanded via `dirs::home_dir()`) —
  `Exclude` never gets suggestions, since those entries are bare directory names to skip
  anywhere in the tree, not filesystem paths. `App::save_config` diffs the draft's
  `include`/`exclude` against the last-saved `Config` to decide `ConfigUpdate.index_now`, writes
  the new config to `~/.scout.toml` via `config::save_config`, updates `App.config`, and sends
  the `ConfigUpdate` down `config_tx` — always leaving the config screen on success regardless
  of whether anything actually changed.
- `run.rs`: owns the crossterm terminal lifecycle (raw mode + alternate screen + mouse capture,
  restored on the way out) and the event loop, with key handling branched per `Screen`. Unlike a
  blocking `event::read()`, the loop calls `terminal.draw`, drains any pending `IndexingEvent`s
  off `index_rx` via `on_indexing_event`, then `event::poll(POLL_INTERVAL)` (200ms) before
  reading — so indexing status updates get drawn promptly even when the user isn't pressing
  anything, not just on the next keypress. A left-click on the `Results` screen is translated via
  `App::result_index_at` into a result index, which both selects that row and reveals it in the
  OS file explorer (`opener::reveal_document`) — there's no separate "select via click" gesture,
  since Up/Down + Enter already cover plain selection. On `Home`, every printable key edits the
  query (no Tab/Navigating there, since there's nothing to navigate yet) and Enter only
  transitions to `Results` if the trimmed query is non-empty; F1/F2 open the `Config`/`Errors`
  screens from either `Home` or `Results`. On `Results`, Enter re-searches while `Searching`, or
  reveals the selected document (same `reveal_document` as the mouse click) while `Navigating`.
  `'q'` only requests the exit confirmation while `Action::Navigating` — while `Searching`, it's
  just a character, otherwise queries containing the letter "q" would be untypeable; `Esc` is the
  always-available way to open the exit confirmation from `Home`/`Results`. On the `Exit` screen,
  Left/Right/Tab toggles which button (`ExitChoice::Yes`/`No`) is highlighted and Enter acts on
  it; `'y'`/`'n'` remain as direct shortcuts. On `Config`, key handling further branches on
  `ConfigDraft.focus`: while just navigating a list, `a` starts adding, `e`/Enter starts editing
  the selected entry, `d`/Delete removes it, `s` saves (persists + notifies the background
  thread) and Esc cancels back to `previous_screen`; while typing into `AddInput`, Tab
  autocompletes (`Include` only), Enter confirms (an empty input removes the entry if this was
  an edit, otherwise is dropped), and Esc cancels just the input. On `Errors`, Up/Down move the
  selection and Enter/Esc return to `previous_screen`.
- `ui.rs`: `draw` dispatches on `app.screen`; the `Exit` overlay first redraws whichever screen
  is in `app.previous_screen` underneath itself, then paints the popup on top via `Clear` + a
  small fixed-size centered rect (`centered_rect_fixed` — deliberately not
  percentage-of-parent, so it doesn't balloon on large terminals). `draw_home` renders "SCOUT" as
  large pixel-art text via the `tui-big-text` crate (`PixelSize::Full`) above a centered search
  box and an `index_status_text` line (shared with `draw_footer` on `Results`, so both screens
  describe the background walk — `IndexStatus::Pending`/`Indexing`/`Done { errors }` — the same
  way). `draw_results_screen` renders a small "SCOUT" label pinned top-left next to the search
  box, the results `List` (with the selected row highlighted only when `Action::Navigating`;
  `app.results_area` is recorded on every draw, even when empty, so a click while there are no
  results reliably misses rather than hit-testing a stale area), and a footer whose help text
  depends on `Action` and whose top-right corner shows `index_status_text`. Each result row
  spans three lines (name, path, then kind/extension/size/modified via `format.rs`, the kind tag
  colored per `theme::kind_color`) — `App`'s `RESULT_ITEM_HEIGHT` constant must stay in sync with
  this for mouse hit-testing to line up. `draw_config_screen` renders `Include`/`Exclude` as two
  side-by-side lists (`draw_config_list`, shared for both) with a help footer that changes text
  depending on `ConfigFocus`; while `AddInput` is active, an inline text box (prefixed `Add>` or
  `Edit>`) is drawn in place of the list's own hint line, wrapping and growing upward as needed,
  with a suggestions line above it when autocomplete candidates exist. `draw_errors_screen` lists
  the last walk's per-file errors (path + `DirErrors`'s `Display` text) in a navigable `List`.
  Colors are centralized in `theme.rs` rather than inlined per-widget.
- `theme.rs`: a blue-toned palette (`PRIMARY`, `ACCENT`, `SUCCESS`, `WARNING`, `DANGER`, `TEXT`,
  `DIM`) plus style helpers (`focused(bool)`, `dim()`, `text()`, `selected()`, `confirm()`/
  `safe()` for the exit popup's Yes/No buttons, and `kind_color(FileType)` so each result's kind
  tag gets its own accent color) — every bordered block/list/popup in `ui.rs` goes through these
  instead of ad hoc `Style`s, so the look stays consistent.
- `format.rs`: display formatting for result metadata — `size` (bytes -> `"1.5 KB"`, stepping
  through units in `f64` rather than integer division), `modified` (unix seconds -> `"YYYY-MM-DD
  HH:MM"` via the `time` crate, falling back to a placeholder for anything out of range,
  including the `modified: 0` sentinel `util::modified_secs` returns when a file's mtime
  couldn't be read), `extension` (uppercased, no leading dot, `None` when the path has none), and
  `kind_label`. Has unit tests — check these when changing result-metadata formatting.
- `opener.rs`: a thin wrapper around the `opener` crate's `reveal`, used by both the mouse-click
  and Enter-on-`Results` paths to show the selected document in the OS's file explorer.
- `index.rs`: defines `IndexingEvent` (background -> TUI) and `ConfigUpdate` (TUI -> background,
  `{ config, index_now }`).

## Notes for future work

- Content extraction for `Image`/`Binary` files doesn't exist — they're only searchable by
  file name/path, never by content.
- Cross-device/volume moves (a file moved to a different mount point or drive) get a new
  `device` component in its `util::FileId`, so they're indistinguishable from a delete +
  create with the current identifier scheme — the old doc gets pruned by `prune_stale_file`
  and the file is re-indexed under a fresh `doc_id`. Since `include` paths are user-editable
  (arbitrary directories can be added/removed, potentially spanning different disks/mounts),
  this isn't just a rare edge case — a user moving files between two indexed volumes will hit
  it. Left unhandled for now as a known limitation: search results are unaffected (the file is
  still found either way), the cost is re-reading/re-tokenizing large text files instead of a
  cheap `file_id` rewrite, and losing `doc_id` continuity doesn't matter yet since nothing
  references `doc_id` outside the database itself. Revisit if either becomes a real cost —
  e.g. once a feature hangs metadata off `doc_id` (favorites, history) or this shows up as a
  perf problem on large files moved across volumes.
- Opening a document only reveals it in the OS file explorer (`opener::reveal`) — there's no
  "open with the default app" alternative (e.g. a modifier-click or a second shortcut), which
  would need a second `opener` call (`opener::open`) and a key/mouse-gesture decision for how
  the two should coexist.
- `walk_dirs`'s parallel workers all write through the same `stats` tree
  (`Database::bump_stats`/`decrease_stat` in `src/database/repository.rs`, touched by every
  `index_document`/`reindex_text_document`/etc. transaction), which is a read-then-write on a
  handful of shared keys (`n_text_docs`, `n_terms`, …). sled retries a transaction on conflict
  rather than corrupting anything, so this doesn't threaten correctness, but it's a real
  serialization point — expect indexing throughput to scale with worker count for the
  walk/classify/tokenize/read-file work, then taper off on the final commit step. Not worth
  fixing preemptively (e.g. by accumulating stats deltas per-thread and merging once at the
  end) without a measurement showing it's the actual bottleneck — it would add real complexity
  to code that currently guarantees per-document atomicity.
- The config screen's `Include` autocomplete only completes to a full existing subdirectory
  (`ConfigDraft::update_suggestions` filters `std::fs::read_dir` results) — there's no way to
  type a path that doesn't exist yet and have it accepted as a not-yet-created directory, nor
  any validation on save that `Include` entries actually exist or that `Include`/`Exclude`
  don't overlap in a way that makes an entry pointless.
- The `Errors` screen (`Screen::Errors`, F2) always shows only the *last* walk's errors,
  replaced wholesale on every `IndexingEvent::Finished` — there's no history across walks, so a
  transient error (e.g. a file locked by another process during one walk) is indistinguishable
  from a persistent one unless the user happens to check right after it occurs.
- No way to trigger a manual reindex from the TUI outside of saving the config screen with a
  changed `include`/`exclude` (which piggybacks a reindex as a side effect) — a user who just
  wants to force a refresh has to wait out the rest of `REINDEX_INTERVAL` (5 minutes).
