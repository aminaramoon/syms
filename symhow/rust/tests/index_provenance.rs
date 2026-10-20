//! Port of test/unit/test_provenance.cpp.

use std::path::PathBuf;

use symhow::db::WritableDb;
use symhow::index::cuda_support::CudaMode;
use symhow::index::file_registry::SystemFileRows;
use symhow::index::provenance::{
    read_provenance, write_provenance, Provenance, K_WALKER_VERSION,
};

fn temp_db_path() -> PathBuf {
    // Each test gets its own file so the three tests can run in parallel
    // without racing on a shared temp path.
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "symhow_test_provenance_{}_{}.sqlite",
        std::process::id(),
        n
    ))
}

fn fresh_meta_db(path: &PathBuf) -> WritableDb {
    let _ = std::fs::remove_file(path);
    // Create the file first (READ_WRITE | CREATE), matching the C++ helper's
    // sqlite3_open_v2(... SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE).
    let _ = rusqlite::Connection::open(path).unwrap();

    let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    // Minimal stand-in for the real Sourcetrail-schema `meta` table.
    db.exec("CREATE TABLE meta(id INTEGER PRIMARY KEY, key TEXT, value TEXT)")
        .unwrap();
    db
}

#[test]
fn read_provenance_returns_none_when_symhow_provenance_version_is_absent() {
    let path = temp_db_path();
    let db = fresh_meta_db(&path);
    assert!(read_provenance(db.conn()).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn write_read_round_trips_every_field() {
    let path = temp_db_path();
    let mut db = fresh_meta_db(&path);

    let mut prov = Provenance::default();
    prov.symhow_version = "1.2.3".to_string();
    prov.walker_version = K_WALKER_VERSION;
    prov.cdb_path = "/abs/path/compile_commands.json".to_string();
    prov.db_options
        .remaps
        .push(("/old".to_string(), "/new".to_string()));
    prov.db_options.include_globs.push("src/**".to_string());
    prov.db_options.exclude_globs.push("third_party/**".to_string());
    prov.sanitizer_options.drop_arg_patterns.push("-W*".to_string());
    prov.sanitizer_options.extra_args.push("-DFOO=1".to_string());
    prov.sanitizer_options.resource_dir = "/opt/clang/lib/clang/21".to_string();
    prov.sanitizer_options.gcc_toolchain = "/opt/gcc".to_string();
    prov.sanitizer_options.std_fallback = "c++20".to_string();
    prov.sanitizer_options.c_std_fallback = "gnu11".to_string();
    prov.sanitizer_options.cuda_path = "/opt/cuda".to_string();
    prov.sanitizer_options.cuda_mode = CudaMode::NoWrapper;
    prov.system_file_rows = SystemFileRows::All;
    prov.limit = 42;
    prov.repo_root = "/home/user/repo".to_string();
    prov.repo_commit = "deadbeefcafef00d".to_string();
    prov.dirty_paths = vec![
        "/home/user/repo/a.cpp".to_string(),
        "/home/user/repo/b.h".to_string(),
    ];
    prov.indexed_at_utc = "2026-01-01T00:00:00Z".to_string();
    prov.last_update_at_utc = "2026-01-02T00:00:00Z".to_string();

    db.begin().unwrap();
    write_provenance(&db, &prov).unwrap();
    db.commit().unwrap();

    let read_back = read_provenance(db.conn()).unwrap();
    assert_eq!(read_back.symhow_version, prov.symhow_version);
    assert_eq!(read_back.walker_version, prov.walker_version);
    assert_eq!(read_back.cdb_path, prov.cdb_path);
    assert_eq!(read_back.db_options.remaps, prov.db_options.remaps);
    assert_eq!(read_back.db_options.include_globs, prov.db_options.include_globs);
    assert_eq!(read_back.db_options.exclude_globs, prov.db_options.exclude_globs);
    assert_eq!(
        read_back.sanitizer_options.drop_arg_patterns,
        prov.sanitizer_options.drop_arg_patterns
    );
    assert_eq!(
        read_back.sanitizer_options.extra_args,
        prov.sanitizer_options.extra_args
    );
    assert_eq!(
        read_back.sanitizer_options.resource_dir,
        prov.sanitizer_options.resource_dir
    );
    assert_eq!(
        read_back.sanitizer_options.gcc_toolchain,
        prov.sanitizer_options.gcc_toolchain
    );
    assert_eq!(
        read_back.sanitizer_options.std_fallback,
        prov.sanitizer_options.std_fallback
    );
    assert_eq!(
        read_back.sanitizer_options.c_std_fallback,
        prov.sanitizer_options.c_std_fallback
    );
    assert_eq!(
        read_back.sanitizer_options.cuda_path,
        prov.sanitizer_options.cuda_path
    );
    assert_eq!(
        read_back.sanitizer_options.cuda_mode,
        prov.sanitizer_options.cuda_mode
    );
    assert_eq!(read_back.system_file_rows, prov.system_file_rows);
    assert_eq!(read_back.limit, prov.limit);
    assert_eq!(read_back.repo_root, prov.repo_root);
    assert_eq!(read_back.repo_commit, prov.repo_commit);
    assert_eq!(read_back.dirty_paths, prov.dirty_paths);
    assert_eq!(read_back.indexed_at_utc, prov.indexed_at_utc);
    assert_eq!(read_back.last_update_at_utc, prov.last_update_at_utc);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn write_provenance_is_idempotent() {
    let path = temp_db_path();
    let mut db = fresh_meta_db(&path);

    let mut prov = Provenance::default();
    prov.symhow_version = "1.0.0".to_string();
    prov.indexed_at_utc = "2026-01-01T00:00:00Z".to_string();

    db.begin().unwrap();
    write_provenance(&db, &prov).unwrap();
    prov.symhow_version = "2.0.0".to_string();
    write_provenance(&db, &prov).unwrap();
    db.commit().unwrap();

    let mut rows = -1i64;
    db.query(
        "SELECT count(*) FROM meta WHERE key = 'symhow_version'",
        |row| {
            rows = row.get(0)?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(rows, 1);

    let read_back = read_provenance(db.conn()).unwrap();
    assert_eq!(read_back.symhow_version, "2.0.0");

    let _ = std::fs::remove_file(&path);
}