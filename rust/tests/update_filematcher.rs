//! Port of test/unit/test_filematcher.cpp.

use std::collections::HashSet;

use symhow::common::paths::NormalizationCache;
use symhow::index::compile_db::CompileEntry;
use symhow::update::file_matcher::{
    match_changed, match_changed_with, normalize_path, select_entries, select_entries_with, MatchInput,
};
use symhow::update::git_diff::{ChangeKind, ChangedFile};

fn contains(v: &[String], s: &str) -> bool {
    v.iter().any(|x| x == s)
}

fn entry_for(file: &str) -> CompileEntry {
    CompileEntry {
        directory: "/repo".to_string(),
        file: file.to_string(),
        output: String::new(),
        args: vec!["clang++".to_string(), file.to_string()],
    }
}

fn changed(kind: ChangeKind, old: &str, new: &str) -> ChangedFile {
    ChangedFile { kind, old_path: old.to_string(), new_path: new.to_string() }
}

#[test]
fn normalize_path_collapses_dot_segments_for_nonexistent_paths() {
    assert_eq!(normalize_path("/repo/./a/../b.cpp"), "/repo/b.cpp");
    assert_eq!(normalize_path("/repo/b.cpp"), "/repo/b.cpp");
}

#[test]
fn normalization_cache_preserves_spelling_and_is_shared() {
    let mut cache = NormalizationCache::default();
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Modified, "", "/repo/a.cpp")],
        db_file_paths: HashSet::from(["/repo/./a.cpp".to_string()]),
        cdb: vec![entry_for("/repo/./a.cpp"), entry_for("/repo/./a.cpp")],
    };
    let r = match_changed_with(&input, &mut cache);
    assert_eq!(r.changed_in_db, vec!["/repo/./a.cpp".to_string()]);
    assert_eq!(cache.size(), 2);

    let selected = select_entries_with(
        &input.cdb,
        &HashSet::from(["/repo/a.cpp".to_string()]),
        &mut cache,
    );
    assert_eq!(selected.len(), 2);
    assert_eq!(cache.size(), 2);
}

#[test]
fn normalization_cache_is_operation_scoped_and_resolves_symlinks() {
    let dir = std::env::temp_dir().join(format!("symhow_path_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.cpp"), "int a;").unwrap();
    std::fs::write(dir.join("b.cpp"), "int b;").unwrap();
    let link = dir.join("link.cpp");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("a.cpp"), &link).unwrap();

    // On macOS the temp dir itself may be reached through a symlink
    // (/var -> /private/var), so symlink-resolved expectations use the
    // canonical spelling of `dir`; the "missing/../" case is lexical and
    // keeps the raw spelling.
    let canon_dir = dir.canonicalize().unwrap();

    let mut first = NormalizationCache::default();
    assert_eq!(first.resolve(link.to_str().unwrap()), canon_dir.join("a.cpp").to_string_lossy());
    assert_eq!(
        first.resolve(dir.join("./link.cpp").to_str().unwrap()),
        canon_dir.join("a.cpp").to_string_lossy()
    );
    assert_eq!(first.resolve(dir.join("missing/../gone.cpp").to_str().unwrap()), dir.join("gone.cpp").to_string_lossy());

    #[cfg(unix)]
    {
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(dir.join("b.cpp"), &link).unwrap();
        // Memoized for this operation:
        assert_eq!(first.resolve(link.to_str().unwrap()), canon_dir.join("a.cpp").to_string_lossy());
        let mut next = NormalizationCache::default();
        assert_eq!(next.resolve(link.to_str().unwrap()), canon_dir.join("b.cpp").to_string_lossy());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn modified_in_db_goes_to_changed_in_db_with_db_spelling() {
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Modified, "", "/repo/a.cpp")],
        db_file_paths: HashSet::from(["/repo/./a.cpp".to_string()]), // DB spelling with redundant "./"
        cdb: vec![],
    };
    let r = match_changed(&input);
    assert_eq!(r.changed_in_db.len(), 1);
    assert_eq!(r.changed_in_db[0], "/repo/./a.cpp"); // original DB spelling preserved
    assert!(r.new_tus.is_empty());
    assert!(r.deleted.is_empty());
    assert!(r.unmatched.is_empty());
}

#[test]
fn added_matching_cdb_entry_goes_to_new_tus_with_cdb_spelling() {
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Added, "", "/repo/new.cpp")],
        db_file_paths: HashSet::new(),
        cdb: vec![entry_for("/repo/./new.cpp")],
    };
    let r = match_changed(&input);
    assert_eq!(r.new_tus.len(), 1);
    assert_eq!(r.new_tus[0], "/repo/./new.cpp");
    assert!(r.changed_in_db.is_empty());
}

#[test]
fn added_with_neither_goes_to_unmatched() {
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Added, "", "/repo/README.md")],
        db_file_paths: HashSet::new(),
        cdb: vec![],
    };
    let r = match_changed(&input);
    assert_eq!(r.unmatched.len(), 1);
    assert_eq!(r.unmatched[0], "/repo/README.md");
}

#[test]
fn deleted_present_in_db_goes_to_deleted_absent_ignored() {
    let input = MatchInput {
        changes: vec![
            changed(ChangeKind::Deleted, "", "/repo/gone.cpp"),
            changed(ChangeKind::Deleted, "", "/repo/never-indexed.cpp"),
        ],
        db_file_paths: HashSet::from(["/repo/gone.cpp".to_string()]),
        cdb: vec![],
    };
    let r = match_changed(&input);
    assert_eq!(r.deleted.len(), 1);
    assert_eq!(r.deleted[0], "/repo/gone.cpp");
}

#[test]
fn renamed_routes_old_through_deleted_and_new_through_changed() {
    // new path unmatched (no DB row, no CDB entry)
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Renamed, "/repo/old.cpp", "/repo/new.cpp")],
        db_file_paths: HashSet::from(["/repo/old.cpp".to_string()]),
        cdb: vec![],
    };
    let r = match_changed(&input);
    assert!(contains(&r.deleted, "/repo/old.cpp"));
    assert!(contains(&r.unmatched, "/repo/new.cpp"));

    // new path matches a CDB entry -> newTus
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Renamed, "/repo/old.cpp", "/repo/new.cpp")],
        db_file_paths: HashSet::from(["/repo/old.cpp".to_string()]),
        cdb: vec![entry_for("/repo/new.cpp")],
    };
    let r = match_changed(&input);
    assert!(contains(&r.deleted, "/repo/old.cpp"));
    assert!(contains(&r.new_tus, "/repo/new.cpp"));

    // new path already in the DB too -> changedInDb
    let input = MatchInput {
        changes: vec![changed(ChangeKind::Renamed, "/repo/old.cpp", "/repo/new.cpp")],
        db_file_paths: HashSet::from(["/repo/old.cpp".to_string(), "/repo/new.cpp".to_string()]),
        cdb: vec![],
    };
    let r = match_changed(&input);
    assert!(contains(&r.deleted, "/repo/old.cpp"));
    assert!(contains(&r.changed_in_db, "/repo/new.cpp"));
}

#[test]
fn select_entries_filters_by_normalized_file() {
    let entries = vec![entry_for("/repo/a.cpp"), entry_for("/repo/./b.cpp"), entry_for("/repo/c.cpp")];
    let wanted: HashSet<String> = [normalize_path("/repo/a.cpp"), normalize_path("/repo/b.cpp")]
        .into_iter()
        .collect();

    let selected = select_entries(&entries, &wanted);
    assert_eq!(selected.len(), 2);
    let files: Vec<&str> = selected.iter().map(|e| e.file.as_str()).collect();
    assert!(files.contains(&"/repo/a.cpp"));
    assert!(files.contains(&"/repo/./b.cpp"));
}