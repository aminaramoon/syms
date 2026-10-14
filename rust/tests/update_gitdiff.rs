//! Port of test/unit/test_gitdiff.cpp.

use symhow::update::git_diff::{
    parse_name_status_z, parse_status_porcelain_z, status_has_unmerged_entries, ChangeKind,
};

const ROOT: &str = "/repo";

fn nul_join(tokens: &[&str]) -> String {
    let mut out = String::new();
    for t in tokens {
        out.push_str(t);
        out.push('\0');
    }
    out
}

#[test]
fn parse_name_status_z_handles_a_m_d_t() {
    let input = nul_join(&["A", "a.cpp", "M", "b.cpp", "D", "c.cpp", "T", "d.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 4);
    assert_eq!(changes[0].kind, ChangeKind::Added);
    assert_eq!(changes[0].new_path, "/repo/a.cpp");
    assert!(changes[0].old_path.is_empty());
    assert_eq!(changes[1].kind, ChangeKind::Modified);
    assert_eq!(changes[1].new_path, "/repo/b.cpp");
    assert_eq!(changes[2].kind, ChangeKind::Deleted);
    assert_eq!(changes[2].new_path, "/repo/c.cpp");
    assert_eq!(changes[3].kind, ChangeKind::Modified);
    assert_eq!(changes[3].new_path, "/repo/d.cpp");
}

#[test]
fn parse_name_status_z_handles_renames_with_both_paths() {
    let input = nul_join(&["R100", "old.cpp", "new.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Renamed);
    assert_eq!(changes[0].old_path, "/repo/old.cpp");
    assert_eq!(changes[0].new_path, "/repo/new.cpp");
}

#[test]
fn parse_name_status_z_handles_copies_as_added() {
    let input = nul_join(&["C75", "src.cpp", "copy.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Added);
    assert!(changes[0].old_path.is_empty());
    assert_eq!(changes[0].new_path, "/repo/copy.cpp");
}

#[test]
fn parse_name_status_z_handles_paths_with_spaces() {
    let input = nul_join(&["M", "path with spaces.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].new_path, "/repo/path with spaces.cpp");
}

#[test]
fn parse_name_status_z_tolerates_trailing_nul_and_empty_input() {
    let trailing = nul_join(&["A", "a.cpp"]);
    assert_eq!(parse_name_status_z(&trailing, ROOT).len(), 1);
    assert!(parse_name_status_z("", ROOT).is_empty());
}

#[test]
fn parse_name_status_z_normalizes_dot_segments() {
    let input = nul_join(&["M", "./sub/../a.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].new_path, "/repo/a.cpp");
}

#[test]
fn parse_status_porcelain_z_handles_plain_modified() {
    let input = nul_join(&[" M a.cpp"]);
    let paths = parse_status_porcelain_z(&input, ROOT);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/repo/a.cpp");
}

#[test]
fn parse_status_porcelain_z_handles_untracked() {
    let input = nul_join(&["?? new.cpp"]);
    let paths = parse_status_porcelain_z(&input, ROOT);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/repo/new.cpp");
}

#[test]
fn parse_status_porcelain_z_renames_report_only_new_path() {
    let input = nul_join(&["R  new", "old"]);
    let paths = parse_status_porcelain_z(&input, ROOT);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], "/repo/new");
}

#[test]
fn parse_status_porcelain_z_skips_ignored_entries() {
    let input = nul_join(&[" M a.cpp", "!! ignored.o", "?? b.cpp"]);
    let paths = parse_status_porcelain_z(&input, ROOT);
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], "/repo/a.cpp");
    assert_eq!(paths[1], "/repo/b.cpp");
}

#[test]
fn parse_status_porcelain_z_tolerates_empty_input() {
    assert!(parse_status_porcelain_z("", ROOT).is_empty());
}

#[test]
fn parse_name_status_z_maps_u_to_unmerged() {
    let input = nul_join(&["U", "conflicted.cpp"]);
    let changes = parse_name_status_z(&input, ROOT);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, ChangeKind::Unmerged);
    assert_eq!(changes[0].new_path, "/repo/conflicted.cpp");
}

#[test]
fn status_has_unmerged_entries_detects_every_pairing() {
    assert!(status_has_unmerged_entries(&nul_join(&["UU conflicted.cpp"])));
    assert!(status_has_unmerged_entries(&nul_join(&["AU added_by_us.cpp"])));
    assert!(status_has_unmerged_entries(&nul_join(&["UA added_by_them.cpp"])));
    assert!(status_has_unmerged_entries(&nul_join(&["DU deleted_by_us.cpp"])));
    assert!(status_has_unmerged_entries(&nul_join(&["UD deleted_by_them.cpp"])));
}

#[test]
fn status_has_unmerged_entries_false_for_ordinary() {
    assert!(!status_has_unmerged_entries(&nul_join(&[" M a.cpp", "?? b.cpp", "A  c.cpp"])));
    assert!(!status_has_unmerged_entries(""));
}