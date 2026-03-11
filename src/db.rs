use crate::query;
use eyre::Result;
use rusqlite::{params, Connection, Result as RusqliteResult};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CANDIDATES: usize = 4000;
const MAX_RESULTS: usize = 500;
const DB_SCHEMA_VERSION: i32 = 2;
const DEFAULT_PRUNE_BATCH_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, Default)]
pub struct BatchApplyStats {
    pub removed_rows: usize,
    pub upserted_rows: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PruneProgress {
    pub next_cursor: i64,
    pub scanned_rows: usize,
    pub removed_rows: usize,
}

pub fn get_db_path() -> Result<PathBuf> {
    let home_dir = home::home_dir().ok_or_else(|| eyre::eyre!("Could not find home directory"))?;
    let db_dir = home_dir.join(".quickfind");
    std::fs::create_dir_all(&db_dir)?;
    Ok(db_dir.join("db.sqlite"))
}

pub fn get_connection() -> Result<Connection> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path)?;
    Ok(conn)
}

pub fn create_tables(conn: &Connection) -> RusqliteResult<()> {
    ensure_base_files_table(conn)?;

    let current_version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current_version < 2 {
        migrate_to_v2(conn)?;
    }

    conn.execute(&format!("PRAGMA user_version = {}", DB_SCHEMA_VERSION), [])?;
    Ok(())
}

pub fn insert_file(conn: &Connection, path: &str) -> RusqliteResult<usize> {
    let (basename, ext, dir) = extract_path_metadata(path);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    conn.execute(
        "INSERT INTO files (path, basename, ext, dir, mtime, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET
            basename = excluded.basename,
            ext = excluded.ext,
            dir = excluded.dir,
            mtime = excluded.mtime,
            indexed_at = excluded.indexed_at",
        params![path, basename, ext, dir, now, now],
    )
}

pub fn search_files(conn: &Connection, term: &str) -> RusqliteResult<Vec<String>> {
    if term.trim().is_empty() {
        return Ok(vec![]);
    }

    let plan = query::parse_query(term);
    if plan.is_empty() {
        return Ok(vec![]);
    }

    let (prefilter_sql, prefilter_params) = build_prefilter_query(&plan);
    let mut stmt = conn.prepare(&prefilter_sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(prefilter_params.iter()),
        |row| row.get::<_, String>(0),
    )?;

    let mut scored = Vec::new();
    for row in rows {
        let path = row?;
        if query::path_matches_query(&path, &plan) {
            let score = query::score_path(&path, &plan);
            scored.push((path, score));
        }

        if scored.len() >= MAX_CANDIDATES {
            break;
        }
    }

    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });

    Ok(scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(path, _)| path)
        .collect())
}

pub fn remove_file(conn: &Connection, path: &str) -> RusqliteResult<usize> {
    conn.execute("DELETE FROM files WHERE path = ?1", params![path])
}

pub fn remove_files_under_prefix(conn: &Connection, dir_path: &str) -> RusqliteResult<usize> {
    let normalized = dir_path.trim_end_matches('/');
    let prefix = format!("{}/%", normalized);
    conn.execute(
        "DELETE FROM files WHERE path = ?1 OR path LIKE ?2",
        params![normalized, prefix],
    )
}

pub fn prune_missing_files(conn: &Connection) -> RusqliteResult<usize> {
    let mut cursor = 0;
    let mut total_deleted = 0;

    loop {
        let progress = prune_missing_files_incremental(conn, cursor, DEFAULT_PRUNE_BATCH_SIZE)?;
        total_deleted += progress.removed_rows;

        if progress.scanned_rows == 0 {
            break;
        }

        if progress.next_cursor <= cursor {
            break;
        }

        cursor = progress.next_cursor;
    }

    Ok(total_deleted)
}

pub fn prune_missing_files_incremental(
    conn: &Connection,
    cursor: i64,
    batch_size: usize,
) -> RusqliteResult<PruneProgress> {
    let batch_size = batch_size.max(1);

    let mut stmt = conn.prepare(
        "SELECT id, path FROM files
         WHERE id > ?1
         ORDER BY id
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![cursor, batch_size as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut scanned_rows = 0;
    let mut removed_rows = 0;
    let mut next_cursor = cursor;

    for row in rows {
        let (id, path) = row?;
        scanned_rows += 1;
        next_cursor = id;

        if !Path::new(&path).exists() {
            removed_rows += conn.execute("DELETE FROM files WHERE id = ?1", params![id])?;
        }
    }

    if scanned_rows == 0 && cursor > 0 {
        return prune_missing_files_incremental(conn, 0, batch_size);
    }

    Ok(PruneProgress {
        next_cursor,
        scanned_rows,
        removed_rows,
    })
}

pub fn apply_batched_updates(
    conn: &mut Connection,
    removed_paths: &[String],
    upsert_paths: &[String],
) -> RusqliteResult<BatchApplyStats> {
    if removed_paths.is_empty() && upsert_paths.is_empty() {
        return Ok(BatchApplyStats::default());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let tx = conn.transaction()?;
    let mut delete_exact_stmt = tx.prepare("DELETE FROM files WHERE path = ?1")?;
    let mut delete_prefix_stmt = tx.prepare(
        "DELETE FROM files WHERE path = ?1 OR path LIKE ?2",
    )?;
    let mut upsert_stmt = tx.prepare(
        "INSERT INTO files (path, basename, ext, dir, mtime, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET
            basename = excluded.basename,
            ext = excluded.ext,
            dir = excluded.dir,
            mtime = excluded.mtime,
            indexed_at = excluded.indexed_at",
    )?;

    let mut stats = BatchApplyStats::default();

    for path in removed_paths {
        let removed = delete_exact_stmt.execute(params![path])?;
        if removed > 0 {
            stats.removed_rows += removed;
            continue;
        }

        let normalized = path.trim_end_matches('/');
        let prefix = format!("{}/%", normalized);
        stats.removed_rows += delete_prefix_stmt.execute(params![normalized, prefix])?;
    }

    for path in upsert_paths {
        let (basename, ext, dir) = extract_path_metadata(path);
        stats.upserted_rows += upsert_stmt.execute(params![path, basename, ext, dir, now, now])?;
    }

    drop(delete_exact_stmt);
    drop(delete_prefix_stmt);
    drop(upsert_stmt);
    tx.commit()?;

    Ok(stats)
}

fn build_prefilter_query(plan: &query::QueryPlan) -> (String, Vec<String>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    if !plan.include_terms.is_empty() {
        for term in &plan.include_terms {
            clauses.push("LOWER(path) LIKE ?".to_string());
            params.push(format!("%{}%", term));
        }
    } else if !plan.ext_filters.is_empty() {
        let mut ext_clauses = Vec::new();
        for ext in &plan.ext_filters {
            if ext.contains('*') || ext.contains('?') || ext.contains('[') {
                // Wildcard extension filters are resolved in query::path_matches_query.
                // Keep SQL prefilter broad to avoid dropping valid matches.
                continue;
            }
            ext_clauses.push("LOWER(path) LIKE ?".to_string());
            params.push(format!("%{}", ext));
        }
        if !ext_clauses.is_empty() {
            clauses.push(format!("({})", ext_clauses.join(" OR ")));
        }
    } else {
        for glob in &plan.path_globs {
            if let Some(fragment) = longest_glob_literal(glob) {
                clauses.push("LOWER(path) LIKE ?".to_string());
                params.push(format!("%{}%", fragment));
            }
        }
    }

    for term in &plan.exclude_terms {
        clauses.push("LOWER(path) NOT LIKE ?".to_string());
        params.push(format!("%{}%", term));
    }

    let mut sql = "SELECT path FROM files".to_string();
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(&format!(" LIMIT {}", MAX_CANDIDATES));

    (sql, params)
}

fn longest_glob_literal(glob: &str) -> Option<String> {
    glob.split(['*', '?', '[', ']'])
        .map(|part| part.trim_matches('/').to_lowercase())
        .filter(|part| part.len() >= 2)
        .max_by_key(|part| part.len())
}

fn extract_path_metadata(path: &str) -> (String, String, String) {
    let normalized = Path::new(path);
    let basename = normalized
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let ext = normalized
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s.to_lowercase()))
        .unwrap_or_default();
    let dir = normalized
        .parent()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    (basename, ext, dir)
}

fn ensure_base_files_table(conn: &Connection) -> RusqliteResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE
         )",
        [],
    )?;
    Ok(())
}

fn migrate_to_v2(conn: &Connection) -> RusqliteResult<()> {
    add_column_if_missing(conn, "files", "basename", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "files", "ext", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "files", "dir", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "files", "mtime", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "files", "indexed_at", "INTEGER NOT NULL DEFAULT 0")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS usage_stats (
             file_id INTEGER PRIMARY KEY,
             open_count INTEGER NOT NULL DEFAULT 0,
             last_opened_at INTEGER,
             FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
         )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_files_basename ON files(basename)",
        [],
    )?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_files_ext ON files(ext)", [])?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_files_dir ON files(dir)", [])?;

    backfill_metadata_columns(conn)?;

    Ok(())
}

fn backfill_metadata_columns(conn: &Connection) -> RusqliteResult<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut stmt = conn.prepare("SELECT id, path FROM files")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        Ok((id, path))
    })?;

    for row in rows {
        let (id, path) = row?;
        let (basename, ext, dir) = extract_path_metadata(&path);

        conn.execute(
            "UPDATE files
             SET basename = CASE WHEN basename = '' THEN ?1 ELSE basename END,
                 ext = CASE WHEN ext = '' THEN ?2 ELSE ext END,
                 dir = CASE WHEN dir = '' THEN ?3 ELSE dir END,
                 indexed_at = CASE WHEN indexed_at = 0 THEN ?4 ELSE indexed_at END,
                 mtime = CASE WHEN mtime = 0 THEN ?4 ELSE mtime END
             WHERE id = ?5",
            params![basename, ext, dir, now, id],
        )?;
    }

    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> RusqliteResult<()> {
    if !column_exists(conn, table, column)? {
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition),
            [],
        )?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> RusqliteResult<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_tables_applies_schema_version_and_columns() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        create_tables(&conn).expect("schema creation should succeed");

        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version should be readable");
        assert_eq!(version, DB_SCHEMA_VERSION);

        assert!(column_exists(&conn, "files", "basename").expect("column check"));
        assert!(column_exists(&conn, "files", "ext").expect("column check"));
        assert!(column_exists(&conn, "files", "dir").expect("column check"));

        let usage_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='usage_stats'",
                [],
                |row| row.get(0),
            )
            .expect("usage_stats lookup should succeed");
        assert_eq!(usage_exists, 1);
    }

    #[test]
    fn create_tables_migrates_legacy_schema_without_data_loss() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        conn.execute(
            "CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE
            )",
            [],
        )
        .expect("legacy table should be created");
        conn.execute("INSERT INTO files (path) VALUES ('/tmp/demo/file.tsx')", [])
            .expect("legacy row should insert");

        create_tables(&conn).expect("migration should succeed");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .expect("row count should be readable");
        assert_eq!(count, 1);

        let basename: String = conn
            .query_row("SELECT basename FROM files LIMIT 1", [], |row| row.get(0))
            .expect("basename should be backfilled");
        assert_eq!(basename, "file.tsx");

        let ext: String = conn
            .query_row("SELECT ext FROM files LIMIT 1", [], |row| row.get(0))
            .expect("ext should be backfilled");
        assert_eq!(ext, ".tsx");
    }

    #[test]
    fn insert_file_populates_metadata_columns() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        create_tables(&conn).expect("schema creation should succeed");

        insert_file(&conn, "/tmp/sample/demo.tsx").expect("insert should succeed");

        let (basename, ext, dir): (String, String, String) = conn
            .query_row(
                "SELECT basename, ext, dir FROM files WHERE path = '/tmp/sample/demo.tsx'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("metadata should be queryable");

        assert_eq!(basename, "demo.tsx");
        assert_eq!(ext, ".tsx");
        assert_eq!(dir, "/tmp/sample");
    }

    #[test]
    fn remove_files_under_prefix_cleans_nested_rows() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        create_tables(&conn).expect("schema creation should succeed");

        insert_file(&conn, "/repo/src/app.tsx").expect("insert should succeed");
        insert_file(&conn, "/repo/src/lib/core.ts").expect("insert should succeed");
        insert_file(&conn, "/repo/tests/app.tsx").expect("insert should succeed");

        let removed =
            remove_files_under_prefix(&conn, "/repo/src").expect("prefix removal should succeed");
        assert_eq!(removed, 2);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .expect("remaining count should be readable");
        assert_eq!(remaining, 1);
    }

    #[test]
    fn apply_batched_updates_uses_single_transactional_pass() {
        let mut conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        create_tables(&conn).expect("schema creation should succeed");

        insert_file(&conn, "/repo/src/a.ts").expect("seed should insert");
        insert_file(&conn, "/repo/src/b.ts").expect("seed should insert");

        let removed = vec!["/repo/src/a.ts".to_string(), "/repo/ghost".to_string()];
        let upserts = vec!["/repo/src/c.tsx".to_string(), "/repo/src/b.ts".to_string()];

        let stats = apply_batched_updates(&mut conn, &removed, &upserts)
            .expect("batch apply should succeed");

        assert!(stats.removed_rows >= 1);
        assert_eq!(stats.upserted_rows, 2);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .expect("count should be readable");
        assert_eq!(count, 2);
    }

    #[test]
    fn prune_missing_files_incremental_scans_in_chunks() {
        let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
        create_tables(&conn).expect("schema creation should succeed");

        insert_file(&conn, "/definitely/missing/one.rs").expect("insert should succeed");
        insert_file(&conn, "/definitely/missing/two.rs").expect("insert should succeed");

        let first = prune_missing_files_incremental(&conn, 0, 1).expect("first prune should work");
        assert_eq!(first.scanned_rows, 1);
        assert_eq!(first.removed_rows, 1);

        let second = prune_missing_files_incremental(&conn, first.next_cursor, 4)
            .expect("second prune should work");
        assert!(second.removed_rows <= 1);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .expect("count should be readable");
        assert_eq!(remaining, 0);
    }
}

