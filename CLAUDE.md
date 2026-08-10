# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Scout is a local file search engine written in Rust. It recursively indexes files from
user-configured directories, tokenizes text content, and stores an inverted index in an
embedded `sled` database for BM-25-style search. A `ratatui` TUI is planned as the frontend
(module currently a stub).

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
        for each file: classify -> process_text_file -> tokenizer -> db.index_document
```

### `config`
`src/config/load.rs` defines `Config { indexing: Indexing { include, exclude } }`, loaded
from `~/.scout.toml` (created with defaults on first run if missing, then validated).
Defaults index `~/Documents` and exclude common noise dirs (`node_modules`, `.git`, `target`,
etc.). The sled database itself lives under the OS data dir (`dirs::data_dir()/scout/index.db`),
separate from the config file.

### `database` (`src/database/`)
Wraps a `sled::Db` with four trees, defined in `schemas.rs`:
- `metadata`: doc_id (u64 BE bytes) -> `Metadata` (path, size, mtime, `FileType`, doc_length)
- `file_names`: file name -> `Vec<doc_id>` (multiple docs can share a file name)
- `terms`: term -> `Vec<TermFrequency>` (doc_id + frequency), i.e. the inverted index
- `stats`: aggregate counters (`n_docs`, `n_terms`) used for BM-25 normalization

`Database::index_document` (`repository.rs`) is the single write path: it generates a doc id
via sled's atomic counter, then writes metadata/file_names/terms/stats inside **one sled
transaction** across all four trees, so a document is indexed atomically or not at all.
`DatabaseError` wraps `sled::Error` and `serde_json::Error`; note the two-layer transaction
error handling — `ConflictableTransactionError` inside the closure vs. `TransactionError`
once it escapes `.transaction(...)`.

### `indexer` (`src/indexer/`)
- `file_walker.rs`: `walk_dirs` uses `walkdir::WalkDir` per configured include directory,
  filtering out excluded directory names, and returns a `Vec<(PathBuf, DirErrors)>` of
  per-file/per-dir errors rather than failing the whole run (permission errors, symlink
  loops, I/O errors, and per-file indexing errors are all collected, not fatal).
- `classifier.rs` / `extension_map.rs`: classifies a path as `Text`/`Image`/`Binary`, first
  by extension lookup (`EXTENSION_MAP`), falling back to content sniffing via
  `content_inspector::inspect` on the first 8KB when the extension is unknown.
- `processors.rs`: `process_text_file` reads a text file line-by-line, tokenizes it, builds
  metadata (using Unix-specific `MetadataExt` for size/mtime — Unix-only as written), counts
  term frequencies, and calls `db.index_document`. Only `FileType::Text` files are processed;
  `Image`/`Binary` are currently skipped entirely during the walk.
- `tokenizer.rs`: lowercases, strips accents (`unicode-normalization` NFD + combining-mark
  filtering), splits on non-alphanumeric (except `_`), and filters a combined
  Spanish/English stopword list. Has unit tests covering these behaviors — check these when
  changing tokenization rules.

### `search` and `tui`
Both `src/search/mod.rs` and `src/tui/mod.rs` are currently empty stubs — not yet implemented.

## Notes for future work

- Only `FileType::Text` documents are indexed; no content extraction exists yet for
  images/binaries.
- The indexing pipeline (walker -> classifier -> processor -> db) has no incremental/
  re-indexing logic yet — every run walks and indexes from scratch.
- `processors.rs` uses `std::os::unix::fs::MetadataExt`, so the indexer as written is
  Unix-only.
