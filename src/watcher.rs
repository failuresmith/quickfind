use crate::config::Config;
use crate::{db, indexing};
use eyre::{eyre, Result};
use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(800);

pub fn run_watcher(conn: &rusqlite::Connection, config: &Config, verbose: bool) -> Result<()> {
    let include_paths = if config.include.is_empty() {
        vec![".".to_string()]
    } else {
        config.include.clone()
    };

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
            flush_pending(conn, config, &mut pending, verbose)?;
        }
    }
}

fn flush_pending(
    conn: &rusqlite::Connection,
    config: &Config,
    pending: &mut PendingChanges,
    verbose: bool,
) -> Result<()> {
    let snapshot = pending.drain();

    for removed in snapshot.removed_paths {
        if removed.is_file() {
            if let Some(p) = removed.to_str() {
                let _ = db::remove_file(conn, p)?;
            }
        } else if removed.is_dir() {
            if let Some(p) = removed.to_str() {
                let _ = db::remove_files_under_prefix(conn, p)?;
            }
        }
    }

    for changed in snapshot.changed_paths {
        if changed.is_file() {
            if let Some(p) = changed.to_str() {
                let _ = db::insert_file(conn, p)?;
            }
        }
    }

    for root in snapshot.roots_to_reindex {
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
                    self.changed_paths.insert(path.clone());
                    if path.is_dir() {
                        self.roots_to_reindex.insert(path);
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
