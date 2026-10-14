//! Port of test/unit/test_compiledb_dupes.cpp.

use symhow::index::compile_db::{path_args_hashes, CompileEntry};

fn make_entry(file: &str, args: Vec<&str>) -> CompileEntry {
    CompileEntry {
        directory: "/work".to_string(),
        file: file.to_string(),
        output: String::new(),
        args: args.into_iter().map(String::from).collect(),
    }
}

fn is_hex16(s: &str) -> bool {
    s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn single_entry_path_gets_a_16_hex_char_hash_and_entry_count_1() {
    let entries = vec![make_entry(
        "/work/a.cpp",
        vec!["/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"],
    )];
    let result = path_args_hashes(&entries);
    assert_eq!(result.len(), 1);
    let info = result.get("/work/a.cpp").unwrap();
    assert!(is_hex16(&info.combined_hash));
    assert_eq!(info.entry_count, 1);
}

#[test]
fn duplicated_paths_combined_hash_is_independent_of_entry_order() {
    let e1 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"]);
    let e2 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DBAR=2", "-c", "a.cpp"]);

    let forward = vec![e1.clone(), e2.clone()];
    let reversed = vec![e2, e1];

    let forward_result = path_args_hashes(&forward);
    let reversed_result = path_args_hashes(&reversed);

    assert_eq!(forward_result.len(), 1);
    assert_eq!(reversed_result.len(), 1);

    let forward_info = forward_result.get("/work/a.cpp").unwrap();
    let reversed_info = reversed_result.get("/work/a.cpp").unwrap();

    assert_eq!(forward_info.combined_hash, reversed_info.combined_hash);
    assert_eq!(forward_info.entry_count, 2);
    assert_eq!(reversed_info.entry_count, 2);
    assert!(is_hex16(&forward_info.combined_hash));
}

#[test]
fn combined_hash_changes_when_any_one_entrys_flags_change() {
    let e1 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"]);
    let e2 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DBAR=2", "-c", "a.cpp"]);
    let e2_changed = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DBAR=3", "-c", "a.cpp"]);

    let before = path_args_hashes(&[e1.clone(), e2]);
    let after = path_args_hashes(&[e1, e2_changed]);

    assert_ne!(
        before.get("/work/a.cpp").unwrap().combined_hash,
        after.get("/work/a.cpp").unwrap().combined_hash
    );
    // entry_count is unaffected -- still two entries either way.
    assert_eq!(before.get("/work/a.cpp").unwrap().entry_count, 2);
    assert_eq!(after.get("/work/a.cpp").unwrap().entry_count, 2);
}

#[test]
fn two_entries_with_byte_identical_args_still_count_as_entry_count_2() {
    let e1 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"]);
    let e2 = make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-DFOO=1", "-c", "a.cpp"]);

    let result = path_args_hashes(&[e1.clone(), e2]);
    assert_eq!(result.len(), 1);
    assert_eq!(result.get("/work/a.cpp").unwrap().entry_count, 2);

    // The combined hash must equal the single-entry case's hash (deduping two
    // identical hashes reduces to hashing just one string either way).
    let single = path_args_hashes(&[e1]);
    assert_eq!(
        result.get("/work/a.cpp").unwrap().combined_hash,
        single.get("/work/a.cpp").unwrap().combined_hash
    );
}

#[test]
fn distinct_paths_never_collide_and_each_keeps_its_own_count() {
    let entries = vec![
        make_entry("/work/a.cpp", vec!["/usr/bin/c++", "-c", "a.cpp"]),
        make_entry("/work/b.cpp", vec!["/usr/bin/c++", "-c", "b.cpp"]),
        make_entry("/work/b.cpp", vec!["/usr/bin/c++", "-DX=1", "-c", "b.cpp"]),
    ];
    let result = path_args_hashes(&entries);
    assert_eq!(result.len(), 2);
    assert_eq!(result.get("/work/a.cpp").unwrap().entry_count, 1);
    assert_eq!(result.get("/work/b.cpp").unwrap().entry_count, 2);
    assert_ne!(
        result.get("/work/a.cpp").unwrap().combined_hash,
        result.get("/work/b.cpp").unwrap().combined_hash
    );
}