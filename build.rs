//! Compile-time SQL gate.
//!
//! Every statement the app runs lives in `db/schema.sql` / `db/queries/` and
//! is embedded via `include_str!`. This script applies the schema to an
//! in-memory database and `EXPLAIN`s each query file: `EXPLAIN` parses the
//! statement and resolves every table/column without executing anything, so
//! a typo'd column or a schema drift **fails the build** instead of first
//! boot or first request.
//!
//! Deliberately no `DATABASE_URL`, no live database file, no offline cache:
//! the schema files *are* the contract, and they travel with the source
//! (including into the Docker builder).

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=db/schema.sql");
    println!("cargo:rerun-if-changed=db/queries");

    let schema_path = PathBuf::from("db/schema.sql");
    let schema = std::fs::read_to_string(&schema_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", schema_path.display()));

    let conn = rusqlite::Connection::open_in_memory().expect("open :memory: database");
    if let Err(e) = conn.execute_batch(&schema) {
        panic!("db/schema.sql is invalid: {e}");
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir("db/queries")
        .expect("cannot read db/queries")
        .map(|entry| entry.expect("cannot read db/queries entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "db/queries contains no .sql files to validate"
    );

    for path in &files {
        let sql = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        assert!(!sql.trim().is_empty(), "{} is empty", path.display());
        // Placeholders (?1, ?2, …) need no bindings at prepare time.
        if let Err(e) = conn.prepare(&format!("EXPLAIN {sql}")) {
            panic!("{} is invalid SQL: {e}", path.display());
        }
    }
}
