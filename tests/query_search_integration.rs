use quickfind::db;
use rusqlite::Connection;

fn setup_conn_with_paths(paths: &[&str]) -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
    db::create_tables(&conn).expect("schema should be created");

    for path in paths {
        db::insert_file(&conn, path).expect("fixture path should be inserted");
    }

    conn
}

#[test]
fn ext_alias_and_dot_extension_return_same_results() {
    let conn = setup_conn_with_paths(&[
        "/work/src/main.rs",
        "/work/src/lib.rs",
        "/work/README.md",
        "/work/web/App.tsx",
    ]);

    let with_dot = db::search_files(&conn, ".rs").expect("search should work");
    let with_ext = db::search_files(&conn, "ext:rs").expect("search should work");

    assert_eq!(with_dot, with_ext);
    assert_eq!(with_dot.len(), 2);
    assert!(with_dot.iter().all(|p| p.ends_with(".rs")));
}

#[test]
fn path_glob_scopes_results() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/ui/App.tsx",
        "/repo/src/core/lib.tsx",
        "/repo/tests/App.tsx",
        "/repo/src/ui/App.rs",
    ]);

    let scoped = db::search_files(&conn, "/src/*.tsx").expect("search should work");

    assert_eq!(scoped.len(), 2);
    assert!(scoped.contains(&"/repo/src/core/lib.tsx".to_string()));
    assert!(scoped.contains(&"/repo/src/ui/App.tsx".to_string()));
}

#[test]
fn negation_excludes_matching_items() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/parser.rs",
        "/repo/src/parser_test.rs",
        "/repo/src/runtime.rs",
    ]);

    let filtered = db::search_files(&conn, "parser !test").expect("search should work");

    assert_eq!(filtered, vec!["/repo/src/parser.rs"]);
}

#[test]
fn ranking_prefers_filename_match_over_path_only_match() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/parser.rs",
        "/repo/notes/about_parser_design.txt",
    ]);

    let ranked = db::search_files(&conn, "parser").expect("search should work");

    assert_eq!(ranked[0], "/repo/src/parser.rs");
}

#[test]
fn search_results_are_capped_for_tui_responsiveness() {
    let conn = Connection::open_in_memory().expect("in-memory sqlite should open");
    db::create_tables(&conn).expect("schema should be created");

    for i in 0..700 {
        let path = format!("/repo/src/file_{i}.rs");
        db::insert_file(&conn, &path).expect("fixture path should be inserted");
    }

    let results = db::search_files(&conn, "ext:rs").expect("search should work");
    assert_eq!(results.len(), 500);
    assert!(results.iter().all(|p| p.ends_with(".rs")));
}

#[test]
fn extension_and_directory_scope_work_together() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/ui/App.tsx",
        "/repo/src/core/lib.tsx",
        "/repo/tests/App.tsx",
        "/repo/src/core/lib.rs",
    ]);

    let results = db::search_files(&conn, ".tsx /src").expect("search should work");

    assert_eq!(results.len(), 2);
    assert!(results.contains(&"/repo/src/ui/App.tsx".to_string()));
    assert!(results.contains(&"/repo/src/core/lib.tsx".to_string()));
}

#[test]
fn star_wildcard_matches_any_length_before_extension() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/ui/App.tsx",
        "/repo/src/ui/Index.tsx",
        "/repo/src/ui/App.ts",
        "/repo/src/ui/App.tsxx",
    ]);

    let results = db::search_files(&conn, "*.tsx").expect("search should work");

    assert_eq!(results.len(), 2);
    assert!(results.contains(&"/repo/src/ui/App.tsx".to_string()));
    assert!(results.contains(&"/repo/src/ui/Index.tsx".to_string()));
}

#[test]
fn question_mark_wildcard_matches_single_character_only() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/file.tsx",
        "/repo/src/file.tsa",
        "/repo/src/file.ts",
        "/repo/src/file.tsxx",
    ]);

    let results = db::search_files(&conn, "*.ts?").expect("search should work");

    assert_eq!(results.len(), 2);
    assert!(results.contains(&"/repo/src/file.tsx".to_string()));
    assert!(results.contains(&"/repo/src/file.tsa".to_string()));
}

#[test]
fn question_mark_works_without_star_prefix() {
    let conn = setup_conn_with_paths(&[
        "/repo/src/file.tsx",
        "/repo/src/file.tsa",
        "/repo/src/file.ts",
        "/repo/src/file.tsxx",
    ]);

    let results = db::search_files(&conn, ".ts?").expect("search should work");

    assert_eq!(results.len(), 2);
    assert!(results.contains(&"/repo/src/file.tsx".to_string()));
    assert!(results.contains(&"/repo/src/file.tsa".to_string()));
}
