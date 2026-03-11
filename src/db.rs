use crate::query;
use eyre::Result;
use rusqlite::{params, Connection, Result as RusqliteResult};
use std::path::PathBuf;

const MAX_CANDIDATES: usize = 4000;
const MAX_RESULTS: usize = 500;

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
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE
         )",
        [],
    )?;
    Ok(())
}

pub fn insert_file(conn: &Connection, path: &str) -> RusqliteResult<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO files (path) VALUES (?1)",
        params![path],
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
    glob.split(|c| matches!(c, '*' | '?' | '[' | ']'))
        .map(|part| part.trim_matches('/').to_lowercase())
        .filter(|part| part.len() >= 2)
        .max_by_key(|part| part.len())
}

