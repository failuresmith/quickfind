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
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(800);
const PRUNE_BATCH_SIZE: usize = 256;
const INITIAL_PRUNE_INTERVAL: Duration = Duration::from_secs(30);
const MIN_PRUNE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_PRUNE_INTERVAL: Duration = Duration::from_secs(180);
const METRICS_REPORT_INTERVAL: Duration = Duration::from_secs(10);
const BACKPRESSURE_WARN_THRESHOLD: usize = 2000;
const BACKPRESSURE_LOG_INTERVAL: Duration = Duration::from_secs(5);
const SPOOL_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const SPOOL_REPLAY_BATCH: usize = 8;
const SPOOL_SEGMENT_EXT: &str = "qfsp";

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

    let pending_ram_hard_cap_bytes = config
        .watch_pending_ram_cap_mb
        .max(1)
        .saturating_mul(1024)
        .saturating_mul(1024);

    let mut pending = PendingChanges::default();
    let mut spill_queue = SpillQueue::new(SPOOL_MAX_BYTES)?;
    let mut prune_state = PruneState::default();
    let mut metrics = WatchMetrics::default();

    let replayed_at_boot = replay_spill_budget(
        conn,
        config,
        &include_roots,
        &ignore_patterns,
        &mut spill_queue,
        &mut metrics,
        verbose,
        usize::MAX,
    )?;
    if verbose && replayed_at_boot > 0 {
        eprintln!("watch replay: recovered {replayed_at_boot} spool segments at startup");
    }

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

            let _ = replay_spill_budget(
                conn,
                config,
                &include_roots,
                &ignore_patterns,
                &mut spill_queue,
                &mut metrics,
                verbose,
                usize::MAX,
            )?;

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

                if pending.estimated_bytes() >= pending_ram_hard_cap_bytes {
                    handle_pending_overflow(
                        &include_roots,
                        &mut pending,
                        &mut spill_queue,
                        &mut metrics,
                        pending_ram_hard_cap_bytes,
                        verbose,
                    )?;
                }
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

        if pending.is_empty() {
            let _ = replay_spill_budget(
                conn,
                config,
                &include_roots,
                &ignore_patterns,
                &mut spill_queue,
                &mut metrics,
                verbose,
                SPOOL_REPLAY_BATCH,
            )?;
        }

        prune_state.maybe_prune(conn, pending.total_paths(), &mut metrics, verbose)?;
        metrics.maybe_report(verbose, pending.total_paths());
    }
}

fn handle_pending_overflow(
    include_roots: &[PathBuf],
    pending: &mut PendingChanges,
    spill_queue: &mut SpillQueue,
    metrics: &mut WatchMetrics,
    pending_ram_hard_cap_bytes: usize,
    verbose: bool,
) -> Result<()> {
    let snapshot = pending.drain();
    if snapshot.is_empty() {
        return Ok(());
    }

    let snapshot_paths = snapshot.total_paths();
    match spill_queue.enqueue_snapshot(&snapshot)? {
        SpillEnqueueOutcome::Empty => {}
        SpillEnqueueOutcome::Enqueued => {
            metrics.record_spill_enqueued(snapshot_paths, verbose);
        }
        SpillEnqueueOutcome::CapacityExceeded => {
            metrics.record_overflow_fallback(
                snapshot_paths,
                pending_ram_hard_cap_bytes,
                verbose,
            );
            for root in include_roots {
                pending.insert_root_to_reindex(root.clone());
            }
        }
    }

    Ok(())
}

fn replay_spill_budget(
    conn: &mut rusqlite::Connection,
    config: &Config,
    include_roots: &[PathBuf],
    ignore_patterns: &[Pattern],
    spill_queue: &mut SpillQueue,
    metrics: &mut WatchMetrics,
    verbose: bool,
    budget: usize,
) -> Result<usize> {
    let mut replayed = 0;

    while replayed < budget {
        let Some(segment_path) = spill_queue.oldest_segment_path()? else {
            break;
        };

        match spill_queue.load_snapshot(&segment_path) {
            Ok(snapshot) => {
                let replayed_paths = snapshot.total_paths();
                let flush_stats = apply_snapshot(
                    conn,
                    config,
                    include_roots,
                    ignore_patterns,
                    snapshot,
                    verbose,
                )?;
                metrics.record_flush(flush_stats, verbose);
                metrics.record_spill_replayed(replayed_paths, verbose);
                spill_queue.remove_segment(&segment_path)?;
            }
            Err(err) => {
                eprintln!("watch spill replay error (quarantine): {err}");
                metrics.record_spill_error(verbose);
                spill_queue.quarantine_segment(&segment_path)?;
            }
        }

        replayed += 1;
    }

    Ok(replayed)
}

fn flush_pending(
    conn: &mut rusqlite::Connection,
    config: &Config,
    include_roots: &[PathBuf],
    ignore_patterns: &[Pattern],
    pending: &mut PendingChanges,
    verbose: bool,
) -> Result<FlushStats> {
    let snapshot = pending.drain();

    apply_snapshot(
        conn,
        config,
        include_roots,
        ignore_patterns,
        snapshot,
        verbose,
    )
}

fn apply_snapshot(
    conn: &mut rusqlite::Connection,
    config: &Config,
    include_roots: &[PathBuf],
    ignore_patterns: &[Pattern],
    snapshot: PendingSnapshot,
    verbose: bool,
) -> Result<FlushStats> {
    let started_at = Instant::now();

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
    approx_bytes: usize,
}

impl PendingChanges {
    fn push(&mut self, event: Event) {
        self.queue.push_back(Instant::now());

        for path in event.paths {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => {
                    if path.exists() {
                        self.insert_changed_path(path.clone());
                        if path.is_dir() {
                            self.insert_root_to_reindex(path);
                        }
                    } else {
                        self.insert_removed_path(path.clone());
                        self.remove_changed_path(&path);
                        self.remove_root_to_reindex(&path);
                    }
                }
                EventKind::Remove(_) => {
                    self.insert_removed_path(path.clone());
                    self.remove_changed_path(&path);
                    self.remove_root_to_reindex(&path);
                }
                EventKind::Any | EventKind::Access(_) | EventKind::Other => {
                    if path.exists() {
                        self.insert_changed_path(path);
                    }
                }
            }
        }
    }

    fn insert_changed_path(&mut self, path: PathBuf) {
        if self.changed_paths.insert(path.clone()) {
            self.approx_bytes = self
                .approx_bytes
                .saturating_add(estimate_path_bytes(&path));
        }
    }

    fn remove_changed_path(&mut self, path: &Path) {
        if self.changed_paths.remove(path) {
            self.approx_bytes = self
                .approx_bytes
                .saturating_sub(estimate_path_bytes(path));
        }
    }

    fn insert_removed_path(&mut self, path: PathBuf) {
        if self.removed_paths.insert(path.clone()) {
            self.approx_bytes = self
                .approx_bytes
                .saturating_add(estimate_path_bytes(&path));
        }
    }

    fn insert_root_to_reindex(&mut self, root: PathBuf) {
        if self.roots_to_reindex.insert(root.clone()) {
            self.approx_bytes = self
                .approx_bytes
                .saturating_add(estimate_path_bytes(&root));
        }
    }

    fn remove_root_to_reindex(&mut self, root: &Path) {
        if self.roots_to_reindex.remove(root) {
            self.approx_bytes = self
                .approx_bytes
                .saturating_sub(estimate_path_bytes(root));
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

    fn estimated_bytes(&self) -> usize {
        self.approx_bytes
            .saturating_add(self.queue.len().saturating_mul(std::mem::size_of::<Instant>()))
    }

    fn drain(&mut self) -> PendingSnapshot {
        self.queue.clear();
        self.approx_bytes = 0;
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

impl PendingSnapshot {
    fn is_empty(&self) -> bool {
        self.changed_paths.is_empty()
            && self.removed_paths.is_empty()
            && self.roots_to_reindex.is_empty()
    }

    fn total_paths(&self) -> usize {
        self.changed_paths.len() + self.removed_paths.len() + self.roots_to_reindex.len()
    }
}

fn estimate_path_bytes(path: &Path) -> usize {
    path.to_string_lossy().len().saturating_add(96)
}

enum SpillEnqueueOutcome {
    Empty,
    Enqueued,
    CapacityExceeded,
}

#[derive(Default)]
struct SpillSnapshot {
    changed_paths: Vec<String>,
    removed_paths: Vec<String>,
    roots_to_reindex: Vec<String>,
}

impl SpillSnapshot {
    fn from_pending(snapshot: &PendingSnapshot) -> Self {
        Self {
            changed_paths: snapshot
                .changed_paths
                .iter()
                .filter_map(|p| p.to_str().map(|s| s.to_string()))
                .collect(),
            removed_paths: snapshot
                .removed_paths
                .iter()
                .filter_map(|p| p.to_str().map(|s| s.to_string()))
                .collect(),
            roots_to_reindex: snapshot
                .roots_to_reindex
                .iter()
                .filter_map(|p| p.to_str().map(|s| s.to_string()))
                .collect(),
        }
    }

    fn into_pending(self) -> PendingSnapshot {
        PendingSnapshot {
            changed_paths: self.changed_paths.into_iter().map(PathBuf::from).collect(),
            removed_paths: self.removed_paths.into_iter().map(PathBuf::from).collect(),
            roots_to_reindex: self.roots_to_reindex.into_iter().map(PathBuf::from).collect(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();

        for path in &self.changed_paths {
            out.extend_from_slice(b"U\0");
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }
        for path in &self.removed_paths {
            out.extend_from_slice(b"D\0");
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }
        for path in &self.roots_to_reindex {
            out.extend_from_slice(b"R\0");
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }

        out
    }

    fn decode(buf: &[u8]) -> Result<Self> {
        let mut changed_paths = Vec::new();
        let mut removed_paths = Vec::new();
        let mut roots_to_reindex = Vec::new();

        let mut parts = buf.split(|b| *b == 0);
        while let Some(tag) = parts.next() {
            if tag.is_empty() {
                continue;
            }

            let Some(path_bytes) = parts.next() else {
                return Err(eyre!("corrupt spill segment: missing record payload"));
            };

            let path = String::from_utf8_lossy(path_bytes).to_string();
            match tag {
                b"U" => changed_paths.push(path),
                b"D" => removed_paths.push(path),
                b"R" => roots_to_reindex.push(path),
                _ => return Err(eyre!("corrupt spill segment: unknown tag")),
            }
        }

        Ok(Self {
            changed_paths,
            removed_paths,
            roots_to_reindex,
        })
    }
}

struct SpillQueue {
    dir: PathBuf,
    max_bytes: u64,
    seq: u64,
}

impl SpillQueue {
    fn new(max_bytes: u64) -> Result<Self> {
        let home = home::home_dir().ok_or_else(|| eyre!("Could not find home directory"))?;
        let dir = home.join(".quickfind").join("watch_spool");
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            max_bytes,
            seq: 0,
        })
    }

    #[cfg(test)]
    fn for_dir(dir: PathBuf, max_bytes: u64) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            max_bytes,
            seq: 0,
        })
    }

    fn enqueue_snapshot(&mut self, snapshot: &PendingSnapshot) -> Result<SpillEnqueueOutcome> {
        if snapshot.is_empty() {
            return Ok(SpillEnqueueOutcome::Empty);
        }

        let spill = SpillSnapshot::from_pending(snapshot);
        let payload = spill.encode();
        if payload.is_empty() {
            return Ok(SpillEnqueueOutcome::Empty);
        }

        let current_size = self.current_size_bytes()?;
        if current_size.saturating_add(payload.len() as u64) > self.max_bytes {
            return Ok(SpillEnqueueOutcome::CapacityExceeded);
        }

        self.seq = self.seq.saturating_add(1);
        let now_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let file_name = format!("{now_nanos:020}_{:08}.{}", self.seq, SPOOL_SEGMENT_EXT);
        let tmp = self.dir.join(format!("{file_name}.tmp"));
        let final_path = self.dir.join(file_name);

        let mut f = File::create(&tmp)?;
        f.write_all(&payload)?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, final_path)?;

        Ok(SpillEnqueueOutcome::Enqueued)
    }

    fn oldest_segment_path(&self) -> Result<Option<PathBuf>> {
        let mut files = self.segment_files()?;
        files.sort();
        Ok(files.into_iter().next())
    }

    fn load_snapshot(&self, path: &Path) -> Result<PendingSnapshot> {
        let mut buf = Vec::new();
        let mut f = File::open(path)?;
        f.read_to_end(&mut buf)?;
        let spill = SpillSnapshot::decode(&buf)?;
        Ok(spill.into_pending())
    }

    fn remove_segment(&self, path: &Path) -> Result<()> {
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn quarantine_segment(&self, path: &Path) -> Result<()> {
        let bad = path.with_extension("bad");
        if path.exists() {
            fs::rename(path, bad)?;
        }
        Ok(())
    }

    fn current_size_bytes(&self) -> Result<u64> {
        let mut total = 0_u64;
        for p in self.segment_files()? {
            total = total.saturating_add(fs::metadata(p)?.len());
        }
        Ok(total)
    }

    fn segment_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension() == Some(OsStr::new(SPOOL_SEGMENT_EXT)) {
                files.push(path);
            }
        }
        Ok(files)
    }
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
    spill_enqueued_segments: usize,
    spill_enqueued_paths: usize,
    spill_replayed_segments: usize,
    spill_replayed_paths: usize,
    spill_replay_errors: usize,
    overflow_fallbacks: usize,
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
            spill_enqueued_segments: 0,
            spill_enqueued_paths: 0,
            spill_replayed_segments: 0,
            spill_replayed_paths: 0,
            spill_replay_errors: 0,
            overflow_fallbacks: 0,
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

    fn record_spill_enqueued(&mut self, paths: usize, verbose: bool) {
        self.spill_enqueued_segments += 1;
        self.spill_enqueued_paths += paths;
        if verbose {
            eprintln!("watch spill: enqueued paths={paths}");
        }
    }

    fn record_spill_replayed(&mut self, paths: usize, verbose: bool) {
        self.spill_replayed_segments += 1;
        self.spill_replayed_paths += paths;
        if verbose {
            eprintln!("watch spill: replayed paths={paths}");
        }
    }

    fn record_spill_error(&mut self, verbose: bool) {
        self.spill_replay_errors += 1;
        if verbose {
            eprintln!("watch spill: replay error (segment quarantined)");
        }
    }

    fn record_overflow_fallback(
        &mut self,
        paths: usize,
        pending_ram_hard_cap_bytes: usize,
        verbose: bool,
    ) {
        self.overflow_fallbacks += 1;
        eprintln!(
            "watch overflow fallback: pending paths={paths}, coarse reindex markers were scheduled"
        );
        if verbose {
            eprintln!(
                "watch overflow fallback detail: RAM cap={} bytes",
                pending_ram_hard_cap_bytes
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
            "watch metrics: events_total={}, events_per_sec={:.2}, pending_paths={}, flushes={}, last_flush_ms={}, prunes={}, last_prune_ms={}, removed_rows_total={}, upserted_rows_total={}, reindexed_roots_total={}, spill_enqueued_segments={}, spill_replayed_segments={}, spill_replay_errors={}, overflow_fallbacks={}",
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
            self.spill_enqueued_segments,
            self.spill_replayed_segments,
            self.spill_replay_errors,
            self.overflow_fallbacks,
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, ModifyKind};

    struct TempDirGuard {
        path: PathBuf,
    }

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let mut path = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should work for temp naming")
                .as_nanos();
            path.push(format!("quickfind_{prefix}_{}_{}", std::process::id(), nanos));
            fs::create_dir_all(&path).expect("temp dir should be created");
            Self { path }
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn mk_modify_event(path: PathBuf) -> Event {
        Event {
            kind: EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            paths: vec![path],
            attrs: Default::default(),
        }
    }

    #[test]
    fn pending_changes_dedup_keeps_estimated_size_bounded() {
        let mut pending = PendingChanges::default();
        let p = PathBuf::from("/tmp/quickfind-dedup-test.txt");

        for _ in 0..2000 {
            pending.push(mk_modify_event(p.clone()));
        }

        assert!(pending.total_paths() <= 1);
        assert!(pending.estimated_bytes() < 32 * 1024);
    }

    #[test]
    fn spill_snapshot_roundtrip_preserves_paths() {
        let snapshot = PendingSnapshot {
            changed_paths: [PathBuf::from("/tmp/a.txt")].into_iter().collect(),
            removed_paths: [PathBuf::from("/tmp/b.txt")].into_iter().collect(),
            roots_to_reindex: [PathBuf::from("/tmp/dir")].into_iter().collect(),
        };

        let encoded = SpillSnapshot::from_pending(&snapshot).encode();
        let decoded = SpillSnapshot::decode(&encoded)
            .expect("decode should succeed")
            .into_pending();

        assert_eq!(decoded.changed_paths, snapshot.changed_paths);
        assert_eq!(decoded.removed_paths, snapshot.removed_paths);
        assert_eq!(decoded.roots_to_reindex, snapshot.roots_to_reindex);
    }

    #[test]
    fn spill_queue_respects_capacity_limit() {
        let temp = TempDirGuard::new("spill_cap");
        let mut queue = SpillQueue::for_dir(temp.path.clone(), 32).expect("queue init should work");

        let snapshot = PendingSnapshot {
            changed_paths: [PathBuf::from("/tmp/this-path-is-long-enough-to-overflow-cap.txt")]
                .into_iter()
                .collect(),
            removed_paths: HashSet::new(),
            roots_to_reindex: HashSet::new(),
        };

        let outcome = queue
            .enqueue_snapshot(&snapshot)
            .expect("enqueue should return an outcome");
        assert!(matches!(outcome, SpillEnqueueOutcome::CapacityExceeded));
    }
}
