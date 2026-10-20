//! Verifies the C++ SourcetrailDB writer bridge (ffi/symhow_srctrldb_bridge.cpp)
//! works end-to-end: write a small hierarchy, then read back the on-disk
//! Sourcetrail schema with rusqlite. Mirrors test/unit/test_writer_smoke.cpp
//! (minus recordSymbolKind/recordSymbolDefinitionKind, which the Rust index
//! layer applies via its own SQL like SrctrldbBackend::flushPending does).

use std::ffi::CString;

use symhow::db::name_decode::decode_serialized_name;
use symhow::srctrldb::{
    serialize_name_hierarchy, ReferenceKindInt, SymhowNameElement, SymhowSourceRange, Writer,
};

/// Builds C-owned name elements, keeping the backing CStrings alive.
struct NameHolder {
    _backing: Vec<CString>,
    elements: Vec<SymhowNameElement>,
}

impl NameHolder {
    fn new(elems: &[(&str, &str, &str)]) -> NameHolder {
        let mut backing = Vec::new();
        let mut elements = Vec::new();
        for (prefix, name, postfix) in elems {
            let p = CString::new(*prefix).unwrap();
            let n = CString::new(*name).unwrap();
            let po = CString::new(*postfix).unwrap();
            elements.push(SymhowNameElement {
                prefix: p.as_ptr(),
                name: n.as_ptr(),
                postfix: po.as_ptr(),
            });
            backing.push(p);
            backing.push(n);
            backing.push(po);
        }
        NameHolder { _backing: backing, elements }
    }
}

fn db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("symhow_writer_smoke_{}.srctrldb", std::process::id()))
}

fn query_scalar(conn: &rusqlite::Connection, sql: &str) -> String {
    conn.query_row(sql, [], |row| row.get::<_, String>(0)).unwrap()
}

fn query_count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |row| row.get(0))
        .unwrap()
}

#[test]
fn writer_records_a_symbol_hierarchy_that_survives_a_round_trip() {
    let path = db_path();
    let _ = std::fs::remove_file(&path);
    let project_path = path.with_extension("srctrlprj");
    let _ = std::fs::remove_file(&project_path);

    let _ = {
        let writer = Writer::new();
        writer.open(path.to_str().unwrap());
        assert!(writer.last_error().is_empty());

        assert!(writer.begin_transaction());

        let file_id = writer.record_file("/nonexistent/example.cpp");
        assert_ne!(file_id, 0);
        assert!(writer.record_file_language(file_id, "cpp"));

        // namespace myns
        let ns = NameHolder::new(&[("", "myns", "")]);
        let ns_id = writer.record_symbol("::", &ns.elements);
        assert_ne!(ns_id, 0);

        // void myns::foo()
        let foo = NameHolder::new(&[("", "myns", ""), ("void ", "foo", "()")]);
        let foo_id = writer.record_symbol("::", &foo.elements);
        assert_ne!(foo_id, 0);
        assert!(writer.record_symbol_location(foo_id, SymhowSourceRange { file_id, start_line: 3, start_column: 1, end_line: 3, end_column: 10 }));

        // void myns::bar(), which calls foo()
        let bar = NameHolder::new(&[("", "myns", ""), ("void ", "bar", "()")]);
        let bar_id = writer.record_symbol("::", &bar.elements);
        assert_ne!(bar_id, 0);
        assert!(writer.record_symbol_location(bar_id, SymhowSourceRange { file_id, start_line: 6, start_column: 1, end_line: 6, end_column: 10 }));

        let reference_id = writer.record_reference(bar_id, foo_id, ReferenceKindInt::Call);
        assert_ne!(reference_id, 0);
        assert!(writer.record_reference_location(reference_id, SymhowSourceRange { file_id, start_line: 7, start_column: 5, end_line: 7, end_column: 10 }));

        assert!(writer.commit_transaction());
        assert!(writer.close());
        assert!(writer.last_error().is_empty());
    };

    assert!(path.exists());

    let conn = rusqlite::Connection::open(&path).unwrap();

    assert_eq!(query_scalar(&conn, "SELECT value FROM meta WHERE key = 'storage_version';"), "25");

    // The 13 expected Sourcetrail-schema tables.
    let expected = [
        "meta", "element", "element_component", "edge", "node", "symbol", "file", "filecontent",
        "local_symbol", "source_location", "occurrence", "component_access", "error",
    ];
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table';")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<_>>();
    tables.sort();
    for t in expected {
        assert!(tables.contains(&t.to_string()), "missing table {}", t);
    }

    // Nodes: myns, myns::foo, myns::bar, file = 4.
    assert_eq!(query_count(&conn, "node"), 4);
    // Edges: MEMBER(myns->foo), MEMBER(myns->bar), CALL(bar->foo) = 3.
    assert_eq!(query_count(&conn, "edge"), 3);
    // element = 4 nodes + 3 edges = 7.
    assert_eq!(query_count(&conn, "element"), 7);
    // source locations: foo, bar, reference = 3.
    assert_eq!(query_count(&conn, "source_location"), 3);
    assert_eq!(query_count(&conn, "occurrence"), 3);
    assert_eq!(query_count(&conn, "file"), 1);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&project_path);
}

#[test]
fn serialize_name_hierarchy_round_trips_through_decode() {
    let holder = NameHolder::new(&[("", "myns", ""), ("void ", "foo", "()")]);
    let serialized = serialize_name_hierarchy("::", &holder.elements);
    let decoded = decode_serialized_name(&serialized);
    assert_eq!(decoded.name_delimiter, "::");
    assert_eq!(decoded.elements.len(), 2);
    assert_eq!(decoded.elements[0].name, "myns");
    assert_eq!(decoded.elements[1].prefix, "void ");
    assert_eq!(decoded.elements[1].name, "foo");
    assert_eq!(decoded.elements[1].postfix, "()");
    assert_eq!(decoded.qualified_name(), "myns::foo()");
}