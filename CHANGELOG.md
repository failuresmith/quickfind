## Changelog

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
