# quickfind

**Search files instantly: configurable, interactive, Rust-powered.**

[![asciicast](https://asciinema.org/a/dLkoml2XselK9G31oqEtv3O0J.svg)](https://asciinema.org/a/dLkoml2XselK9G31oqEtv3O0J)

Remember part of a filename? Find it instantly in milliseconds, open it in your default app or jump straight into `vim`.

## Install Quickly

```bash
$ cargo install quickfind
```

<details> <summary>Usage</summary>

## 1. Index once

```bash
$ quickfind --index
```

## 2. Search any moment

```bash
$ quickfind <your-query>

# OR

$ quickfind
```

## 3. Keep index in sync continuously (watch mode)

```bash
$ quickfind --watch
```

### Optional fallback for unreliable native FS events

```bash
$ quickfind --watch --watch-poll

# tune poll interval (ms)
$ quickfind --watch --watch-poll --watch-poll-interval-ms 400
```
</details> 

---


<details> <summary>Why quickfind?</summary>

Since I started using Linux, I always felt one essential tool was missing: a fast, reliable file finder like _Everything Search_ on Windows.  
So I built **quickfind** in Rust. Its configurable indexing and interactive TUI make finding files fast, reliable, and effortless.

</details>

<details> <summary>Features</summary>

- **Configurable:** Customize search locations, ignored paths, and search depth via a simple config file.
- **Efficient Indexing:** Traverses directories once and stores paths in a local database for lightning-fast searching.
- **Background Sync:** `--watch` mode keeps the index updated in near real-time as files change.
- **Polling Fallback:** `--watch-poll` provides cross-filesystem resilience when native notifications are unreliable.
- **Interactive Interface:** Browse results with a minimal TUI, open files in default apps or `vim`.

</details>

<details> <summary>Install from Source</summary>
1. Clone the repository:

```bash
$ git clone https://github.com/0xsecaas/quickfind
```

2. Build the project:

```bash
$ cd quickfind
$ cargo build --release
```

3. Run the application:

```bash
$ ./target/release/quickfind

# OR

$ cargo run 
```

</details> 

<details> <summary>Configuration</summary>

Config file: `~/.quickfind/conf.toml`

```toml
include = [
    "/path/to/your/directory",
    "/another/path/to/search"
]
ignore = [
    "**/node_modules/**",
    "**/.git/**",
]
depth = 10
editor = "vim" # "vi" or "code" or "subl" or any editor of your choice
highlight_color = "lightblue"
```

- `include`: Absolute paths to directories you want to index.
- `ignore`: Glob patterns for paths to exclude.
- `depth`: Maximum directory depth to traverse.
- `editor`: Preferred editor for opening selected files from TUI.
- `highlight_color`: Optional result highlight color in TUI.
</details> 

<details> <summary>Interactive Mode</summary>

- `Tab`: Switch between search input and results
- `Arrow Keys`: Navigate results
- `Enter`: Open selected file/directory with default app
- `v`: Open selected file with vim
- `d`: Open containing directory
- `Esc`: Exit interactive mode

</details> 

<details> <summary>Architecture</summary>

- `main.rs`: CLI parsing and orchestration
- `config.rs`: Loads and manages user configs (`~/.quickfind/conf.toml`)
- `db.rs`: Handles persistent file indexing storage
- `indexing.rs`: Traverses directories and populates the database
- `watcher.rs`: Filesystem watcher loop, batching, pruning, and sync logic
- `tui.rs`: Interactive Text User Interface

</details> 

<details> <summary>Future Plans</summary>

- **Fuzzy Search Mode**: typo-tolerant and ranking-aware matching
- **Usage-Aware Ranking**: prioritize frequently opened files

</details> 

<details> <summary>Contributing</summary>

Open issues, submit PRs, or suggest features.

</details> 

<details> <summary>License</summary>

MIT License

</details>
