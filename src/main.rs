mod config;
mod db;
mod indexing;
mod query;
mod tui;
mod watcher;

use clap::Parser;
use eyre::{eyre, Result};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::process::Command;
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
    #[clap(long, action, requires = "watch")]
    watch_poll: bool,

    /// Polling interval in milliseconds (only used with --watch-poll)
    #[clap(long, default_value_t = 700, requires = "watch_poll")]
    watch_poll_interval_ms: u64,

    /// Interactive onboarding to configure include paths and watcher RAM cap
    #[clap(long, action)]
    init: bool,

    /// Full setup wizard: onboarding + initial index + optional daemon install
    #[clap(long, action)]
    setup: bool,
}

fn run_index_once(conn: &rusqlite::Connection, cfg: &config::Config, verbose: bool) -> Result<()> {
    println!("Indexing files...");
    let paths_to_index = if cfg.include.is_empty() {
        vec![".".to_string()]
    } else {
        cfg.include.clone()
    };

    for path in paths_to_index {
        println!("Indexing path: {}", path);
        indexing::index_files(conn, cfg, &path, verbose)?;
    }
    println!("Indexing complete.");
    Ok(())
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{prompt} {hint} ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default_yes);
    }

    Ok(matches!(trimmed, "y" | "Y" | "yes" | "YES" | "Yes"))
}

fn install_daemon_service() -> Result<()> {
    let home = home::home_dir().ok_or_else(|| eyre!("Could not find home directory"))?;
    let systemd_user_dir = home.join(".config").join("systemd").join("user");
    fs::create_dir_all(&systemd_user_dir)?;

    let service_path = systemd_user_dir.join("quickfind-watcher.service");
    let exec = std::env::current_exe()?;
    let service = format!(
        "[Unit]\nDescription=quickfind watcher daemon\nAfter=default.target\n\n[Service]\nType=simple\nExecStart={} --watch\nRestart=on-failure\nRestartSec=2\nNice=19\nIOSchedulingClass=idle\n\n[Install]\nWantedBy=default.target\n",
        exec.display()
    );
    fs::write(&service_path, service)?;

    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", "quickfind-watcher.service"])
        .status();

    match (reload, enable) {
        (Ok(r), Ok(e)) if r.success() && e.success() => {
            println!("Daemon enabled: quickfind-watcher.service");
            println!("Logs: journalctl --user -u quickfind-watcher.service -f");
            Ok(())
        }
        _ => Err(eyre!(
            "failed to enable systemd user daemon; ensure systemd user services are available"
        )),
    }
}

fn run_setup_flow(verbose: bool) -> Result<()> {
    config::run_init_onboarding()?;
    let cfg = config::load_config()?;
    let conn = db::get_connection()?;
    db::create_tables(&conn)?;
    run_index_once(&conn, &cfg, verbose)?;

    if prompt_yes_no(
        "Enable always-on watcher daemon via systemd user service?",
        true,
    )? {
        if let Err(err) = install_daemon_service() {
            eprintln!("daemon setup skipped: {err}");
        }
    } else {
        println!("Skipping daemon setup. You can still run: quickfind --watch");
    }

    println!("Setup complete. Try: quickfind <query>");
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.setup {
        run_setup_flow(cli.verbose)?;
        return Ok(());
    }

    if cli.init {
        config::run_init_onboarding()?;
        return Ok(());
    }

    if !cli.index
        && !cli.watch
        && cli.search_term.is_none()
        && io::stdin().is_terminal()
        && !config::get_config_path()?.exists()
    {
        println!("No quickfind config found. Starting first-run setup...");
        run_setup_flow(cli.verbose)?;
        return Ok(());
    }

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
        run_index_once(&conn, &config, cli.verbose)?;
    } else {
        tui::run_tui(&conn, cli.search_term)?;
    }

    Ok(())
}
