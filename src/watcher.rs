use crate::config::Config;
use crate::{db, indexing};
use eyre::{eyre, Result};
use glob::Pattern;
use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(800);

pub fn run_watcher(conn: &rusqlite::Connection, config: &Config, verbose: bool) -> Result<()> {
    run_watcher_internal(conn, config, verbose, None)
}

#[allow(dead_code)]
pub fn run_watcher_with_stop(
    conn: &rusqlite::Connection,
    config: &Config,
    verbose: bool,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<()> {
    run_watcher_internal(conn, config, verbose, Some(stop_rx))
}

fn run_watcher_internal(
    conn: &rusqlite::Connection,
    config: &Config,
    verbose: bool,
    stop_rx: Option<&mpsc::Receiver<()>>,
) -> Result<()> {
    let include_paths = if config.include.is_empty() {
        vec![".".to_string()]
    } else {
        config.include.clone()
    };
    let include_roots: Vec<PathBuf> = include_paths.iter().map(PathBuf::from).collect();
    let ignore_patterns: Vec<Pattern> = config
        .ignore
        .iter()
        .map(|s| Pattern::new(s))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for path in &include_paths {
        indexing::index_files(conn, config, path, verbose)?;
    }
    let _ = db::prune_missing_files(conn)?;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;

    for path in &include_paths {
        watcher.watch(Path::new(path), RecursiveMode::Recursive)?;
    }

    println!("Watcher active. Press Ctrl+C to stop.");

    let mut pending = PendingChanges::default();

    loop {
        if should_stop(stop_rx) {
            if !pending.is_empty() {
                flush_pending(
                    conn,
                    config,
                    &include_roots,
                    &ignore_patterns,
                    &mut pending,
                    verbose,
                )?;
            }
            return Ok(());
        }

        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => {
                if verbose {
                    eprintln!("watch event: {:?}", event);
                }
                pending.push(event);
            }
            Ok(Err(e)) => {
                eprintln!("watch error: {e}");
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(eyre!("watch channel disconnected")),
        }

        if pending.ready() {
            flush_pending(
                conn,
                config,
                &include_roots,
                &ignore_patterns,
                &mut pending,
                verbose,
            )?;
        }
    }
}

fn flush_pending(
    conn: &rusqlite::Connection,
    config: &Config,
    include_roots: &[PathBuf],
    ignore_patterns: &[Pattern],
    pending: &mut PendingChanges,
    verbose: bool,
) -> Result<()> {
    let snapshot = pending.drain();

    for removed in snapshot.removed_paths {
        if let Some(p) = removed.to_str() {
            let _ = db::remove_file(conn, p)?;
            let _ = db::remove_files_under_prefix(conn, p)?;
        }
    }

    for changed in snapshot.changed_paths {
        if is_ignored(&changed, include_roots, ignore_patterns) {
            continue;
        }

        if changed.is_file() {
            if let Some(p) = changed.to_str() {
                let _ = db::insert_file(conn, p)?;
            }
        }
    }

    for root in snapshot.roots_to_reindex {
        if is_ignored(&root, include_roots, ignore_patterns) {
            continue;
        }

        if let Some(path) = root.to_str() {
            if verbose {
                eprintln!("reindexing scope: {path}");
            }
            indexing::index_files(conn, config, path, false)?;
        }
    }

    let _ = db::prune_missing_files(conn)?;
    Ok(())
}

#[derive(Default)]
struct PendingChanges {
    changed_paths: HashSet<PathBuf>,
    removed_paths: HashSet<PathBuf>,
    roots_to_reindex: HashSet<PathBuf>,
    queue: VecDeque<Instant>,
}

impl PendingChanges {
    fn push(&mut self, event: Event) {
        self.queue.push_back(Instant::now());

        for path in event.paths {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => {
                    if path.exists() {
                        self.changed_paths.insert(path.clone());
                        if path.is_dir() {
                            self.roots_to_reindex.insert(path);
                        }
                    } else {
                        self.removed_paths.insert(path.clone());
                        self.changed_paths.remove(&path);
                    }
                }
                EventKind::Remove(_) => {
                    self.removed_paths.insert(path.clone());
                    self.changed_paths.remove(&path);
                }
                EventKind::Any | EventKind::Access(_) | EventKind::Other => {
                    if path.exists() {
                        self.changed_paths.insert(path);
                    }
                }
            }
        }
    }

    fn ready(&self) -> bool {
        self.queue
            .front()
            .map(|instant| instant.elapsed() >= DEBOUNCE_WINDOW)
            .unwrap_or(false)
    }

    fn is_empty(&self) -> bool {
        self.changed_paths.is_empty()
            && self.removed_paths.is_empty()
            && self.roots_to_reindex.is_empty()
    }

    fn drain(&mut self) -> PendingSnapshot {
        self.queue.clear();
        PendingSnapshot {
            changed_paths: std::mem::take(&mut self.changed_paths),
            removed_paths: std::mem::take(&mut self.removed_paths),
            roots_to_reindex: std::mem::take(&mut self.roots_to_reindex),
        }
    }
}

struct PendingSnapshot {
    changed_paths: HashSet<PathBuf>,
    removed_paths: HashSet<PathBuf>,
    roots_to_reindex: HashSet<PathBuf>,
}

fn should_stop(stop_rx: Option<&mpsc::Receiver<()>>) -> bool {
    let Some(rx) = stop_rx else {
        return false;
    };

    match rx.try_recv() {
        Ok(_) => true,
        Err(mpsc::TryRecvError::Disconnected) => true,
        Err(mpsc::TryRecvError::Empty) => false,
    }
}

fn is_ignored(path: &Path, include_roots: &[PathBuf], ignore_patterns: &[Pattern]) -> bool {
    if ignore_patterns.iter().any(|p| p.matches_path(path)) {
        return true;
    }

    for root in include_roots {
        if let Ok(relative_path) = path.strip_prefix(root) {
            if ignore_patterns.iter().any(|p| p.matches_path(relative_path)) {
                return true;
            }
        }
    }

    false
}
