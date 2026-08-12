# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Scout is a local file search engine written in Rust. It recursively indexes files from
user-configured directories, tokenizes text content, and stores an inverted index in an
embedded `sled` database, searchable via title-prefix match, file-name-token match, and
BM-25 ranking. A `ratatui` TUI is planned as the frontend (module currently a stub).

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
Database::new(db_path)              ->  sled-backed Database
        |
        v
indexer::walk_dirs(include, exclude, &db)
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
search::Searcher::search(query)     ->  title-prefix match, else BM-25 over term tokens + file-name tokens
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
- `file_walker.rs`: `walk_dirs` uses `walkdir::WalkDir` per configured include directory,
  filtering out excluded directory names, and returns a `Vec<(PathBuf, DirErrors)>` of
  per-file/per-dir errors rather than failing the whole run (permission errors, symlink
  loops, I/O errors, and per-file indexing errors are all collected, not fatal). Before
  indexing, `index_state` classifies each file as `New`, `Unchanged`, or `Modified`, primarily
  by computing the file's `util::file_id` and looking it up via
  `Database::get_doc_id_by_file_id`: no match is `New`; a match is `Unchanged` only if both
  the stored path and mtime match the file's current path/mtime, otherwise `Modified` — so a
  bare rename (same content, new path) is classified the same way as a content edit, rather
  than looking like a brand-new file. Falls back to the old path-only comparison (via
  `get_file_modified_time`, blind to renames) when the platform can't provide a `file_id`.
  `New` files go through `index_file` (classify -> process_*), and `Modified` files go through
  `reindex_file` (classify -> process_and_reindex_*). `index_state` also records every file's
  `FileId` (as raw bytes) into a `visited_file_ids` set threaded through the whole walk. Once
  every configured directory has been walked, `walk_dirs` diffs that set against
  `Database::get_all_file_ids` (via `FileId::from_bytes`, which returns `None` — skipped rather
  than erroring — for a key of the wrong length) and calls `prune_stale_file` for every indexed
  `file_id` that wasn't visited: it looks up the doc's old `Metadata` to normalize its file name,
  then calls `db.delete_document`, no-oping rather than erroring if the id or doc has already
  vanished (e.g. a race with another prune). This is how deletes and moves-out-of-scope get
  reflected in the index — a file whose `file_id` is never seen again during a walk is treated
  as gone.
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

### `tui`
`src/tui/mod.rs` is currently an empty stub — not yet implemented.

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
