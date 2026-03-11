# quickfind

**Search files instantly: configurable, interactive, Rust-powered.**

[![asciicast](https://asciinema.org/a/dLkoml2XselK9G31oqEtv3O0J.svg)](https://asciinema.org/a/dLkoml2XselK9G31oqEtv3O0J)

Remember part of a filename? Find it instantly in milliseconds, open it in your default app or jump straight into `vim`.

## Install Quickly

```bash
$ cargo install quickfind
```

## Seamless onboarding (works with `cargo install quickfind`)

After install, just run:

```bash
$ quickfind
```

If this is your first run (no config yet), quickfind automatically starts setup wizard:
- asks your index locations,
- builds initial index,
- optionally enables Linux user daemon.

You can also run setup manually at any time:

```bash
$ quickfind --setup
```

## Quick Start (recommended)

### 1) Pick your locations interactively

```bash
$ quickfind --init
```

This onboarding command asks for directories to index and writes `~/.quickfind/conf.toml`.

### 1-b) Or run full setup in one command

```bash
$ quickfind --setup
```

This runs onboarding + indexing + optional daemon install in one flow.

### 2) Build the index once

```bash
$ quickfind --index
```

### 3) Enable always-on watcher daemon (Linux, user service)

Create this file:

`~/.config/systemd/user/quickfind-watcher.service`

```ini
[Unit]
Description=quickfind watcher daemon
After=default.target

[Service]
Type=simple
ExecStart=/home/YOUR_USER/.cargo/bin/quickfind --watch
Restart=on-failure
RestartSec=2
Nice=19
IOSchedulingClass=idle

[Install]
WantedBy=default.target
```

Then enable it:

```bash
$ systemctl --user daemon-reload
$ systemctl --user enable --now quickfind-watcher.service
$ systemctl --user status quickfind-watcher.service
```

### 4) Search instantly any time

```bash
$ quickfind <your-query>

# OR interactive mode
$ quickfind
```

---

<details> <summary>Advanced Usage</summary>

## Manual watcher mode (foreground)

```bash
$ quickfind --init

$ quickfind --index

$ quickfind --watch
```

## Polling fallback (when native fs events are unreliable)

```bash
$ quickfind --watch --watch-poll

# tune poll interval (ms)
$ quickfind --watch --watch-poll --watch-poll-interval-ms 400
```

## Daemon logs

```bash
$ journalctl --user -u quickfind-watcher.service -f
```

## Disable daemon

```bash
$ systemctl --user disable --now quickfind-watcher.service
```
</details> 

---


<details> <summary>Why quickfind?</summary>

Since I started using Linux, I always felt one essential tool was missing: a fast, reliable file finder like _Everything Search_ on Windows.  
So I built **quickfind** in Rust. Its configurable indexing and interactive TUI make finding files fast, reliable, and effortless.

</details>

<details> <summary>Features</summary>

- **Configurable:** Customize search locations, ignored paths, and search depth via a simple config file.
- **Interactive Onboarding:** `quickfind --init` asks for locations and writes config for you.
- **Efficient Indexing:** Traverses directories once and stores paths in a local database for lightning-fast searching.
- **Background Sync:** `--watch` mode keeps the index updated in near real-time as files change.
- **Polling Fallback:** `--watch-poll` provides cross-filesystem resilience when native notifications are unreliable.
- **Bounded Memory Watcher:** Pending watcher memory is capped (`watch_pending_ram_cap_mb`, default `200`).
- **Spill-to-Disk Backpressure:** Overflow snapshots are persisted to disk and replayed safely.
- **Graceful Degradation:** If both RAM and spool are pressured, watcher falls back to coarse scoped reindex markers (correctness first).
- **Crash-Safe Recovery:** Pending spool segments are replayed on watcher startup.
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
watch_pending_ram_cap_mb = 200
```

- `include`: Absolute paths to directories you want to index.
- `ignore`: Glob patterns for paths to exclude.
- `depth`: Maximum directory depth to traverse.
- `editor`: Preferred editor for opening selected files from TUI.
- `highlight_color`: Optional result highlight color in TUI.
- `watch_pending_ram_cap_mb`: Watcher in-memory pending buffer cap (MB). Default is `200` if omitted.
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
- `config.rs`: Loads/saves config and powers interactive onboarding (`quickfind --init`)
- `db.rs`: Handles persistent file indexing storage
- `indexing.rs`: Traverses directories and populates the database
- `watcher.rs`: Filesystem watcher loop with debounce, batching, adaptive prune, bounded pending queue, spill-to-disk segments, replay/quarantine, and overflow fallback
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
