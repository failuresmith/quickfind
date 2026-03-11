use quickfind::{config::Config, db, watcher};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for test temp naming")
            .as_nanos();
        path.push(format!("quickfind_{prefix}_{}_{}", std::process::id(), nanos));
        fs::create_dir_all(&path).expect("temp test dir should be created");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn watcher_config(root: &Path, ignore: Vec<String>) -> Config {
    Config {
        include: vec![root.to_string_lossy().to_string()],
        ignore,
        depth: 20,
        highlight_color: None,
        editor: None,
        watch_pending_ram_cap_mb: 200,
    }
}

fn all_paths(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .expect("paths statement should prepare");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows should query");

    rows.map(|r| r.expect("row value should decode")).collect()
}

#[test]
fn watcher_handles_create_rename_delete_bursts() {
    let mut conn = Connection::open_in_memory().expect("in-memory sqlite should open");
    db::create_tables(&conn).expect("schema should be initialized");

    let temp = TempDirGuard::new("watch_burst");
    let config = watcher_config(&temp.path, vec![]);

    let (stop_tx, stop_rx) = mpsc::channel();
    let root = temp.path.clone();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));

        let src = root.join("src");
        fs::create_dir_all(&src).expect("src dir should be created");

        for i in 0..12 {
            let p = src.join(format!("file_{i}.tsx"));
            fs::write(&p, format!("payload-{i}"))
                .expect("burst files should be written successfully");
        }

        fs::rename(src.join("file_0.tsx"), src.join("renamed.tsx"))
            .expect("rename should succeed");
        fs::remove_file(src.join("file_1.tsx")).expect("file delete should succeed");

        let removed_dir = src.join("removed_dir");
        fs::create_dir_all(&removed_dir).expect("removed_dir should be created");
        fs::write(removed_dir.join("tmp_a.txt"), "a").expect("tmp file should be created");
        fs::write(removed_dir.join("tmp_b.txt"), "b").expect("tmp file should be created");
        fs::remove_dir_all(&removed_dir).expect("removed_dir should be removed");

        thread::sleep(Duration::from_millis(1500));
        stop_tx.send(()).expect("stop signal should send");
    });

    watcher::run_watcher_with_stop(&mut conn, &config, false, &stop_rx)
        .expect("watcher should run and stop cleanly");
    worker.join().expect("worker thread should join");

    let paths = all_paths(&conn);
    let root_str = temp.path.to_string_lossy();
    let renamed = format!("{root_str}/src/renamed.tsx");
    let old_name = format!("{root_str}/src/file_0.tsx");
    let deleted = format!("{root_str}/src/file_1.tsx");

    assert!(paths.contains(&renamed));
    assert!(!paths.contains(&old_name));
    assert!(!paths.contains(&deleted));
    assert!(
        paths.iter()
            .all(|p| !p.contains("/src/removed_dir/") && !p.ends_with("/src/removed_dir"))
    );
}

#[test]
fn watcher_respects_editor_temp_ignore_patterns() {
    let mut conn = Connection::open_in_memory().expect("in-memory sqlite should open");
    db::create_tables(&conn).expect("schema should be initialized");

    let temp = TempDirGuard::new("watch_editor_tmp");
    let config = watcher_config(
        &temp.path,
        vec![
            "**/*.swp".to_string(),
            "**/*.tmp".to_string(),
            "**/*~".to_string(),
            "**/.*".to_string(),
        ],
    );

    let (stop_tx, stop_rx) = mpsc::channel();
    let root = temp.path.clone();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));

        let work = root.join("work");
        fs::create_dir_all(&work).expect("work dir should be created");

        fs::write(work.join("keep.rs"), "fn main() {}")
            .expect("normal file should be written");
        fs::write(work.join("note.rs.swp"), "swap").expect("swap file should be written");
        fs::write(work.join("draft.tmp"), "tmp").expect("tmp file should be written");
        fs::write(work.join("scratch~"), "backup").expect("backup file should be written");
        fs::write(work.join(".hidden.swp"), "hidden").expect("hidden file should be written");

        thread::sleep(Duration::from_millis(250));
        fs::rename(work.join("draft.tmp"), work.join("draft.rs"))
            .expect("tmp->real rename should succeed");

        thread::sleep(Duration::from_millis(1500));
        stop_tx.send(()).expect("stop signal should send");
    });

    watcher::run_watcher_with_stop(&mut conn, &config, false, &stop_rx)
        .expect("watcher should run and stop cleanly");
    worker.join().expect("worker thread should join");

    let paths = all_paths(&conn);
    let root_str = temp.path.to_string_lossy();
    assert!(paths.contains(&format!("{root_str}/work/keep.rs")));
    assert!(paths.contains(&format!("{root_str}/work/draft.rs")));

    assert!(paths.iter().all(|p| !p.ends_with(".swp")));
    assert!(paths.iter().all(|p| !p.ends_with(".tmp")));
    assert!(paths.iter().all(|p| !p.ends_with('~')));
    assert!(paths.iter().all(|p| !p.contains("/.hidden")));
}

#[test]
fn watcher_poll_backend_syncs_changes() {
    let mut conn = Connection::open_in_memory().expect("in-memory sqlite should open");
    db::create_tables(&conn).expect("schema should be initialized");

    let temp = TempDirGuard::new("watch_poll");
    let config = watcher_config(&temp.path, vec![]);

    let (stop_tx, stop_rx) = mpsc::channel();
    let root = temp.path.clone();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(400));

        fs::write(root.join("poll_file.txt"), "hello").expect("poll file should be written");

        thread::sleep(Duration::from_millis(1500));
        stop_tx.send(()).expect("stop signal should send");
    });

    watcher::run_watcher_with_backend_and_stop(
        &mut conn,
        &config,
        false,
        watcher::WatchBackend::Poll {
            interval: Duration::from_millis(150),
        },
        &stop_rx,
    )
    .expect("watcher poll backend should run");

    worker.join().expect("worker thread should join");

    let paths = all_paths(&conn);
    let root_str = temp.path.to_string_lossy();
    assert!(paths.contains(&format!("{root_str}/poll_file.txt")));
}
