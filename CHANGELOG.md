## Changelog

### v1.5.1

* Added: `--watch` mode for near real-time background index sync using filesystem notifications.
* Added: incremental DB sync helpers for delete/prune workflows (`remove_file`, `remove_files_under_prefix`, `prune_missing_files`).
* Added: watcher event debounce + scoped reindex strategy to keep updates responsive without full reindex each change.

### v1.5.0

* Added: Database schema versioning with migration scaffolding (PRAGMA `user_version`) to support safer future upgrades.
* Added: Metadata columns and migration backfill flow in `files` table (`basename`, `ext`, `dir`, `mtime`, `indexed_at`).
* Added: `usage_stats` table + supporting indexes as roadmap groundwork for future usage-aware ranking.
* Added: migration integrity tests for fresh and legacy database states.

### v1.4.5

* Fix: Wildcard queries now highlight matched result segments in the TUI (e.g. `.ts?` highlights `tsx`).

### v1.4.4

* Fix: `?` wildcard now works independently in extension filters (e.g. `.ts?`) without requiring a leading `*`.

### v1.4.3

* Improved: Wildcard pattern filters now support expected glob semantics, including `*` for any-length and `?` for exactly one character (e.g. `*.ts?`).

### v1.4.2

* Improved: Search input now supports `Ctrl+Shift+Left/Right` to expand or shrink text selection by word boundaries.

### v1.4.1

* Improved: Search input selection now supports `Ctrl+C`, `Ctrl+X`, and `Ctrl+V` clipboard-style editing behavior.

### v1.4.0

* Improved: Added richer result navigation in Results focus (`PageUp`, `PageDown`, `Home`, `End`).
* Improved: Added `Alt+Backspace` in Search focus to delete one word at a time.
* Improved: Added text selection support in Search focus for `Shift+Home` and `Shift+End` with visual highlighting.
* Improved: Query UX now supports combined directory + extension behavior like `.tsx /src`.

### v1.1.1

*   Improved: Enhanced search functionality in `src/db.rs` to support tokenized searches (e.g., "hans mp3" for "Hans Zimmer Time.mp3") and generalized file extension matching (e.g., ".mp3").
* Improved: Choose your desired editor from the `editor` configuration option
* Fix: Keep the count of ignored items.
* Fix: Consider all inclusion paths.
