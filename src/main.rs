mod config;
mod db;
mod indexing;
mod query;
mod tui;
mod watcher;

use clap::Parser;
use eyre::Result;
use std::time::Duration;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    /// The search term
    search_term: Option<String>,

    /// Index files based on the configuration
    #[clap(long, short, action)]
    index: bool,

    /// Enable verbose output
    #[clap(long, short, action)]
    verbose: bool,

    /// Run background watcher for near real-time index sync
    #[clap(long, action)]
    watch: bool,

    /// Use polling watcher backend (fallback for unreliable native fs events)
    #[clap(long, action)]
    watch_poll: bool,

    /// Polling interval in milliseconds (only used with --watch-poll)
    #[clap(long, default_value_t = 700)]
    watch_poll_interval_ms: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::load_config()?;
    let mut conn = db::get_connection()?;
    db::create_tables(&conn)?;

    if cli.watch {
        println!("Starting quickfind watcher...");

        let backend = if cli.watch_poll {
            watcher::WatchBackend::Poll {
                interval: Duration::from_millis(cli.watch_poll_interval_ms.max(100)),
            }
        } else {
            watcher::WatchBackend::Native
        };

        watcher::run_watcher_with_backend(&mut conn, &config, cli.verbose, backend)?;
    } else if cli.index {
        println!("Indexing files...");
        let paths_to_index = if config.include.is_empty() {
            vec![".".to_string()]
        } else {
            config.include.clone()
        };

        for path in paths_to_index {
            println!("Indexing path: {}", path);
            indexing::index_files(&conn, &config, &path, cli.verbose)?;
        }
        println!("Indexing complete.");
    } else {
        tui::run_tui(&conn, cli.search_term)?;
    }

    Ok(())
}
