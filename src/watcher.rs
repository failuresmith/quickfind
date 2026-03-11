use crate::config::Config;
use crate::{db, indexing};
use eyre::{eyre, Result};
use glob::Pattern;
use notify::{
    recommended_watcher, Config as NotifyConfig, Event, EventKind, PollWatcher, RecursiveMode,
    Watcher,
};
use std::collections::{HashSet, VecDeque};
use std::cmp::{max, min};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(800);
const PRUNE_BATCH_SIZE: usize = 256;
const INITIAL_PRUNE_INTERVAL: Duration = Duration::from_secs(30);
const MIN_PRUNE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_PRUNE_INTERVAL: Duration = Duration::from_secs(180);
const METRICS_REPORT_INTERVAL: Duration = Duration::from_secs(10);
const BACKPRESSURE_WARN_THRESHOLD: usize = 2000;
const BACKPRESSURE_LOG_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub enum WatchBackend {
    Native,
    Poll { interval: Duration },
}

impl Default for WatchBackend {
    fn default() -> Self {
        Self::Native
    }
}

#[allow(dead_code)]
pub fn run_watcher(conn: &mut rusqlite::Connection, config: &Config, verbose: bool) -> Result<()> {
    run_watcher_with_backend(conn, config, verbose, WatchBackend::Native)
}

pub fn run_watcher_with_backend(
    conn: &mut rusqlite::Connection,
    config: &Config,
    verbose: bool,
    backend: WatchBackend,
) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&interrupted))?;

    run_watcher_internal(conn, config, verbose, backend, None, Some(interrupted))
}

#[allow(dead_code)]
pub fn run_watcher_with_backend_and_stop(
    conn: &mut rusqlite::Connection,
    config: &Config,
    verbose: bool,
    backend: WatchBackend,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<()> {
    run_watcher_internal(conn, config, verbose, backend, Some(stop_rx), None)
}

#[allow(dead_code)]
pub fn run_watcher_with_stop(
    conn: &mut rusqlite::Connection,
    config: &Config,
    verbose: bool,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<()> {
    run_watcher_internal(
        conn,
        config,
        verbose,
        WatchBackend::Native,
        Some(stop_rx),
        None,
    )
}

fn run_watcher_internal(
    conn: &mut rusqlite::Connection,
    config: &Config,
    verbose: bool,
    backend: WatchBackend,
    stop_rx: Option<&mpsc::Receiver<()>>,
    interrupted: Option<Arc<AtomicBool>>,
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
    let mut watcher = AnyWatcher::new(backend, tx)?;

    for path in &include_paths {
        watcher.watch(Path::new(path), RecursiveMode::Recursive)?;
    }

    println!("Watcher active. Press Ctrl+C to stop.");

    let mut pending = PendingChanges::default();
    let mut prune_state = PruneState::default();
    let mut metrics = WatchMetrics::default();

    loop {
        if should_stop(stop_rx, interrupted.as_deref()) {
            if !pending.is_empty() {
                let flush_stats = flush_pending(
                    conn,
                    config,
                    &include_roots,
                    &ignore_patterns,
                    &mut pending,
                    verbose,
                )?;
                metrics.record_flush(flush_stats, verbose);
            }

            let start = Instant::now();
            let removed = db::prune_missing_files(conn)?;
            metrics.record_prune(start.elapsed(), removed, verbose);
            return Ok(());
        }

        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => {
                if verbose {
                    eprintln!("watch event: {:?}", event);
                }
                metrics.record_event();
                pending.push(event);
            }
            Ok(Err(e)) => {
                eprintln!("watch error: {e}");
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(eyre!("watch channel disconnected")),
        }

        if pending.ready() {
            let flush_stats = flush_pending(
                conn,
                config,
                &include_roots,
                &ignore_patterns,
                &mut pending,
                verbose,
            )?;
            metrics.record_flush(flush_stats, verbose);
        }

        prune_state.maybe_prune(conn, pending.total_paths(), &mut metrics, verbose)?;
        metrics.maybe_report(verbose, pending.total_paths());
    }
}

fn flush_pending(
    conn: &mut rusqlite::Connection,
    config: &Config,
    include_roots: &[PathBuf],
    ignore_patterns: &[Pattern],
    pending: &mut PendingChanges,
    verbose: bool,
) -> Result<FlushStats> {
    let started_at = Instant::now();
    let snapshot = pending.drain();

    let mut removed_paths = Vec::with_capacity(snapshot.removed_paths.len());
    for removed in snapshot.removed_paths {
        if let Some(p) = removed.to_str() {
            removed_paths.push(p.to_string());
        }
    }

    let mut upsert_paths = Vec::with_capacity(snapshot.changed_paths.len());
    for changed in snapshot.changed_paths {
        if is_ignored(&changed, include_roots, ignore_patterns) {
            continue;
        }

        if changed.is_file() {
            if let Some(p) = changed.to_str() {
                upsert_paths.push(p.to_string());
            }
        }
    }

    let apply_stats = db::apply_batched_updates(conn, &removed_paths, &upsert_paths)?;

    let mut reindexed_roots = 0;
    for root in snapshot.roots_to_reindex {
        if is_ignored(&root, include_roots, ignore_patterns) {
            continue;
        }

        if let Some(path) = root.to_str() {
            if verbose {
                eprintln!("reindexing scope: {path}");
            }
            indexing::index_files(conn, config, path, false)?;
            reindexed_roots += 1;
        }
    }

    Ok(FlushStats {
        duration: started_at.elapsed(),
        db_stats: apply_stats,
        reindexed_roots,
    })
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

    fn total_paths(&self) -> usize {
        self.changed_paths.len() + self.removed_paths.len() + self.roots_to_reindex.len()
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

#[derive(Debug, Default)]
struct FlushStats {
    duration: Duration,
    db_stats: db::BatchApplyStats,
    reindexed_roots: usize,
}

struct PruneState {
    cursor: i64,
    interval: Duration,
    last_run_at: Instant,
}

impl Default for PruneState {
    fn default() -> Self {
        Self {
            cursor: 0,
            interval: INITIAL_PRUNE_INTERVAL,
            last_run_at: Instant::now(),
        }
    }
}

impl PruneState {
    fn maybe_prune(
        &mut self,
        conn: &rusqlite::Connection,
        pending_paths: usize,
        metrics: &mut WatchMetrics,
        verbose: bool,
    ) -> Result<()> {
        if self.last_run_at.elapsed() < self.interval {
            return Ok(());
        }

        if pending_paths > BACKPRESSURE_WARN_THRESHOLD {
            return Ok(());
        }

        let started_at = Instant::now();
        let progress = db::prune_missing_files_incremental(conn, self.cursor, PRUNE_BATCH_SIZE)?;
        let elapsed = started_at.elapsed();

        if progress.scanned_rows == 0 {
            self.cursor = 0;
            self.interval = min(MAX_PRUNE_INTERVAL, self.interval + self.interval);
        } else {
            self.cursor = progress.next_cursor;
            if progress.removed_rows > 0 {
                self.interval = max(MIN_PRUNE_INTERVAL, self.interval / 2);
            } else {
                self.interval = min(MAX_PRUNE_INTERVAL, self.interval + Duration::from_secs(5));
            }
        }

        self.last_run_at = Instant::now();
        metrics.record_prune(elapsed, progress.removed_rows, verbose);
        Ok(())
    }
}

struct WatchMetrics {
    window_started_at: Instant,
    last_report_at: Instant,
    last_backpressure_log_at: Instant,
    total_events: usize,
    window_events: usize,
    flush_count: usize,
    last_flush_duration: Duration,
    total_removed_rows: usize,
    total_upserted_rows: usize,
    total_reindexed_roots: usize,
    prune_count: usize,
    last_prune_duration: Duration,
}

impl Default for WatchMetrics {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            window_started_at: now,
            last_report_at: now,
            last_backpressure_log_at: now,
            total_events: 0,
            window_events: 0,
            flush_count: 0,
            last_flush_duration: Duration::from_millis(0),
            total_removed_rows: 0,
            total_upserted_rows: 0,
            total_reindexed_roots: 0,
            prune_count: 0,
            last_prune_duration: Duration::from_millis(0),
        }
    }
}

impl WatchMetrics {
    fn record_event(&mut self) {
        self.total_events += 1;
        self.window_events += 1;
    }

    fn record_flush(&mut self, stats: FlushStats, verbose: bool) {
        self.flush_count += 1;
        self.last_flush_duration = stats.duration;
        self.total_removed_rows += stats.db_stats.removed_rows;
        self.total_upserted_rows += stats.db_stats.upserted_rows;
        self.total_reindexed_roots += stats.reindexed_roots;

        if verbose {
            eprintln!(
                "watch flush: removed_rows={}, upserted_rows={}, reindexed_roots={}, duration_ms={}",
                stats.db_stats.removed_rows,
                stats.db_stats.upserted_rows,
                stats.reindexed_roots,
                stats.duration.as_millis()
            );
        }
    }

    fn record_prune(&mut self, duration: Duration, removed_rows: usize, verbose: bool) {
        self.prune_count += 1;
        self.last_prune_duration = duration;

        if verbose {
            eprintln!(
                "watch prune: removed_rows={}, duration_ms={}",
                removed_rows,
                duration.as_millis()
            );
        }
    }

    fn maybe_report(&mut self, verbose: bool, pending_paths: usize) {
        if pending_paths >= BACKPRESSURE_WARN_THRESHOLD
            && self.last_backpressure_log_at.elapsed() >= BACKPRESSURE_LOG_INTERVAL
        {
            eprintln!(
                "watch backpressure: pending_paths={}, total_events={}, flushes={}",
                pending_paths, self.total_events, self.flush_count
            );
            self.last_backpressure_log_at = Instant::now();
        }

        if !verbose || self.last_report_at.elapsed() < METRICS_REPORT_INTERVAL {
            return;
        }

        let secs = self.window_started_at.elapsed().as_secs_f64().max(0.001);
        let eps = self.window_events as f64 / secs;
        eprintln!(
            "watch metrics: events_total={}, events_per_sec={:.2}, pending_paths={}, flushes={}, last_flush_ms={}, prunes={}, last_prune_ms={}, removed_rows_total={}, upserted_rows_total={}, reindexed_roots_total={}",
            self.total_events,
            eps,
            pending_paths,
            self.flush_count,
            self.last_flush_duration.as_millis(),
            self.prune_count,
            self.last_prune_duration.as_millis(),
            self.total_removed_rows,
            self.total_upserted_rows,
            self.total_reindexed_roots,
        );

        self.window_started_at = Instant::now();
        self.window_events = 0;
        self.last_report_at = Instant::now();
    }
}

enum AnyWatcher {
    Native(notify::RecommendedWatcher),
    Poll(PollWatcher),
}

impl AnyWatcher {
    fn new(backend: WatchBackend, tx: mpsc::Sender<notify::Result<Event>>) -> notify::Result<Self> {
        match backend {
            WatchBackend::Native => {
                let watcher = recommended_watcher(move |res| {
                    let _ = tx.send(res);
                })?;
                Ok(Self::Native(watcher))
            }
            WatchBackend::Poll { interval } => {
                let notify_config = NotifyConfig::default()
                    .with_poll_interval(interval)
                    .with_compare_contents(true);
                let watcher = PollWatcher::new(
                    move |res| {
                        let _ = tx.send(res);
                    },
                    notify_config,
                )?;
                Ok(Self::Poll(watcher))
            }
        }
    }

    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
        match self {
            Self::Native(w) => w.watch(path, mode),
            Self::Poll(w) => w.watch(path, mode),
        }
    }
}

fn should_stop(stop_rx: Option<&mpsc::Receiver<()>>, interrupted: Option<&AtomicBool>) -> bool {
    if interrupted
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
    {
        return true;
    }

    match stop_rx {
        Some(rx) => match rx.try_recv() {
            Ok(_) => true,
            Err(mpsc::TryRecvError::Disconnected) => true,
            Err(mpsc::TryRecvError::Empty) => false,
        },
        None => false,
    }
}

fn is_ignored(path: &Path, include_roots: &[PathBuf], ignore_patterns: &[Pattern]) -> bool {
    if !include_roots.iter().any(|root| path.starts_with(root)) {
        return true;
    }

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
