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
          modified  -> collected as DirErrors::PendingReindex (reindexing not implemented yet)
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
Wraps a `sled::Db` with six trees, defined in `schemas.rs`:
- `metadata`: doc_id (u64 BE bytes) -> `Metadata` (path, size, mtime, `FileType`, doc_length)
- `paths`: path string -> doc_id (u64 BE bytes); lets `get_file_modified_time` look up a
  file's previously recorded mtime by path during the walk, without a doc_id in hand
- `file_names`: normalized file name -> `Vec<doc_id>` (multiple docs can share a file name)
- `terms`: term -> `Vec<TermFrequency>` (doc_id + frequency), i.e. the inverted index over
  text content, populated only for `FileType::Text`
- `name_terms`: file-name token (split on `_` too, via `tokenize_file_name`) -> `Vec<doc_id>`;
  populated for every file regardless of type, so binaries/images are findable by name
- `stats`: aggregate counters (`n_text_docs`, `n_terms`, `n_binary_docs`, `n_image_docs`) used
  for BM-25 normalization via `get_stats` -> `Stats { total_text_docs, avg_total_terms }`

`Database::index_document` and `Database::index_binary_and_image` (`repository.rs`) are the
write paths: each generates a doc id via sled's atomic counter, then writes metadata/paths/
file_names/(terms)/name_terms/stats inside **one sled transaction** across all the trees it
touches, so a document is indexed atomically or not at all. `index_document` additionally
writes to `terms` and bumps `n_text_docs`/`n_terms`; `index_binary_and_image` skips `terms`
and bumps `n_binary_docs`/`n_image_docs` by `FileType`. `DatabaseError` wraps `sled::Error`
and `serde_json::Error`; note the two-layer transaction error handling —
`ConflictableTransactionError` inside the closure vs. `TransactionError` once it escapes
`.transaction(...)`.

### `indexer` (`src/indexer/`)
- `file_walker.rs`: `walk_dirs` uses `walkdir::WalkDir` per configured include directory,
  filtering out excluded directory names, and returns a `Vec<(PathBuf, DirErrors)>` of
  per-file/per-dir errors rather than failing the whole run (permission errors, symlink
  loops, I/O errors, and per-file indexing errors are all collected, not fatal). Before
  indexing, `index_state` compares the file's current mtime against the mtime recorded in
  `Metadata` (looked up via `Database::get_file_modified_time`, through the `paths` tree) to
  classify it as `New`, `Unchanged`, or `Modified`: `Unchanged` files are skipped entirely,
  `New` files go through `index_file` (classify -> process), and `Modified` files are only
  flagged as `DirErrors::PendingReindex` — actually reindexing them (updating the existing
  doc rather than minting a new doc_id) is not implemented yet.
- `classifier.rs` / `extension_map.rs`: classifies a path as `Text`/`Image`/`Binary`, first
  by extension lookup (`EXTENSION_MAP`), falling back to content sniffing via
  `content_inspector::inspect` on the first 8KB when the extension is unknown.
- `processors.rs`: `process_text_file` reads a text file line-by-line, tokenizes it, builds
  metadata (using Unix-specific `MetadataExt` for size/mtime — Unix-only as written), counts
  term frequencies, and calls `db.index_document`. `process_binary_and_image_file` builds
  metadata with `doc_length: 0` and calls `db.index_binary_and_image`, so `Image`/`Binary`
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
- Modified files are detected (`IndexState::Modified` in `file_walker.rs`) but not yet
  reindexed — they're currently just collected as `DirErrors::PendingReindex`. This is the
  next planned piece of work: updating the existing doc (metadata, terms, name_terms, stats)
  for a changed path instead of minting a new doc_id, and pruning stats/inverted-index
  entries for the stale content.
- Deleted files (present in the index but no longer on disk) aren't detected or pruned by
  the walker at all yet.
- Renames/moves aren't detected either, and are effectively a delete + create: since
  `index_state`/`get_file_modified_time` key off the file's path (via the `paths` tree), a
  renamed file's new path has no recorded entry, so it's indexed as `IndexState::New` with a
  fresh `doc_id`, while the old path's `metadata`/`paths`/`terms`/`name_terms` entries are
  left behind pointing at a file that no longer exists there. Fixing this needs an identifier
  stable across renames — the standard approach is the file's inode (`MetadataExt::ino()`,
  already Unix-only like the rest of `processors.rs`) instead of (or alongside) the path, so a
  known inode showing up at a new path is recognized as a rename rather than a new document.
- `processors.rs` uses `std::os::unix::fs::MetadataExt`, so the indexer as written is
  Unix-only.
