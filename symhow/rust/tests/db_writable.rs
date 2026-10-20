//! Port of test/unit/test_writable_db.cpp.

use std::path::PathBuf;

use symhow::db::meta::{delete_meta, get_meta, set_meta};
use symhow::db::WritableDb;

fn create_empty_db(path: &str) {
    // rusqlite's Connection::open uses READ_WRITE | CREATE, matching the C++
    // helper's sqlite3_open_v2(... SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE).
    let _ = rusqlite::Connection::open(path).unwrap();
}

fn temp_db_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn exec_runs_ddl_and_changes_work_against_bound_insert_select() {
    let path = temp_db_path("symhow_test_writable_db_basic.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    db.exec_bound("INSERT INTO t(v) VALUES (?)", ["hello"]).unwrap();
    assert_eq!(db.changes(), 1);

    let mut seen = String::new();
    db.query("SELECT v FROM t", |row| {
        seen = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, "hello");

    assert!(db.table_exists("t"));
    assert!(!db.table_exists("nonexistent_table"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn exec_errors_with_sqlite_message_on_failure() {
    let path = temp_db_path("symhow_test_writable_db_error.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    assert!(db.exec("SELECT * FROM this_table_does_not_exist").is_err());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn begin_commit_persists_begin_rollback_via_drop_discards() {
    let path = temp_db_path("symhow_test_writable_db_txn.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    {
        let mut db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
        db.exec("CREATE TABLE t(v INTEGER)").unwrap();
        db.begin().unwrap();
        db.exec("INSERT INTO t(v) VALUES (1)").unwrap();
        db.commit().unwrap();
    }

    {
        let mut db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
        db.begin().unwrap();
        db.exec("INSERT INTO t(v) VALUES (2)").unwrap();
        // No commit — drop should roll this back.
    }

    {
        let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
        let mut count = 0i64;
        db.query("SELECT count(*) FROM t", |row| {
            count = row.get(0)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn explicit_rollback_discards() {
    let path = temp_db_path("symhow_test_writable_db_rollback.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    let mut db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    db.exec("CREATE TABLE t(v INTEGER)").unwrap();
    db.begin().unwrap();
    db.exec("INSERT INTO t(v) VALUES (1)").unwrap();
    db.rollback().unwrap();

    let mut count = -1i64;
    db.query("SELECT count(*) FROM t", |row| {
        count = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn meta_set_get_overwrite_delete_round_trip() {
    let path = temp_db_path("symhow_test_writable_db_meta.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    db.exec("CREATE TABLE meta(id INTEGER PRIMARY KEY, key TEXT, value TEXT)").unwrap();

    assert!(get_meta(db.conn(), "foo").is_none());

    set_meta(&db, "foo", "bar").unwrap();
    assert_eq!(get_meta(db.conn(), "foo").as_deref(), Some("bar"));

    set_meta(&db, "foo", "baz").unwrap();
    assert_eq!(get_meta(db.conn(), "foo").as_deref(), Some("baz"));

    let mut rows = -1i64;
    db.query_bound("SELECT count(*) FROM meta WHERE key = ?", ["foo"], |row| {
        rows = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(rows, 1);

    delete_meta(&db, "foo").unwrap();
    assert!(get_meta(db.conn(), "foo").is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn foreign_keys_true_enforces_on_delete_cascade() {
    let path = temp_db_path("symhow_test_writable_db_fk_cascade.sqlite");
    let _ = std::fs::remove_file(&path);
    create_empty_db(path.to_str().unwrap());

    let db = WritableDb::open(path.to_str().unwrap(), true).unwrap();
    db.exec("CREATE TABLE parent(id INTEGER PRIMARY KEY)").unwrap();
    db.exec(
        "CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER, \
         FOREIGN KEY(parent_id) REFERENCES parent(id) ON DELETE CASCADE)",
    )
    .unwrap();
    db.exec("INSERT INTO parent(id) VALUES (1)").unwrap();
    db.exec("INSERT INTO child(id, parent_id) VALUES (10, 1)").unwrap();

    let mut before = -1i64;
    db.query("SELECT count(*) FROM child", |row| {
        before = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(before, 1);

    db.exec("DELETE FROM parent WHERE id = 1").unwrap();

    let mut after = -1i64;
    db.query("SELECT count(*) FROM child", |row| {
        after = row.get(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(after, 0);

    let _ = std::fs::remove_file(&path);
}