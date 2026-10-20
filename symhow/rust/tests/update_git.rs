//! Port of test/unit/test_git.cpp: exercises the `git` shell-out wrappers
//! (`symhow::update::git`) against fresh, throwaway git repositories.
//! Each test is skipped when `git` isn't available on PATH (the C++
//! SKIP()s for the same reason).

use std::path::{Path, PathBuf};

use symhow::common::process::run;
use symhow::update::git::{collect_changes, rev_parse, submodule_exclude_globs, submodule_paths, top_level};
use symhow::update::{ChangeKind, ChangeSet};

fn git_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| run(&["git".to_string(), "--version".to_string()], None).exit_code == 0)
}

fn run_checked(argv: &[&str], cwd: &str) {
    let owned: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let result = run(&owned, Some(cwd));
    assert_eq!(result.exit_code, 0, "running {:?} in '{}': {}", argv, cwd, result.err);
}

fn write_file(p: &Path, contents: &str) {
    std::fs::write(p, contents).unwrap();
}

// Fresh git repo under the temp dir, unique per test run (a stale repo from a
// previous run would make these tests non-hermetic).
fn fresh_repo_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("symhow_test_git_repo_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn has_change(changes: &[symhow::update::ChangedFile], kind: ChangeKind, new_path_suffix: &str) -> bool {
    changes.iter().any(|c| c.kind == kind && c.new_path.ends_with(new_path_suffix))
}

#[test]
fn git_collect_changes_working_tree_finds_modified_and_untracked_and_lists_both_dirty() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }

    let repo = fresh_repo_dir("worktree");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);
    run_checked(&["git", "config", "user.email", "symhow-test@example.com"], &repo_str);
    run_checked(&["git", "config", "user.name", "symhow-test"], &repo_str);

    write_file(&repo.join("a.cpp"), "int a();\n");
    write_file(&repo.join("b.cpp"), "int b();\n");
    run_checked(&["git", "add", "-A"], &repo_str);
    run_checked(&["git", "commit", "-q", "-m", "init"], &repo_str);

    // Modify a tracked file, add an untracked one.
    write_file(&repo.join("a.cpp"), "int a(); // changed\n");
    write_file(&repo.join("c.cpp"), "int c();\n");

    let change_set: ChangeSet = collect_changes(&repo_str, "HEAD", None).expect("collect_changes failed");

    assert!(has_change(&change_set.changes, ChangeKind::Modified, "a.cpp"));
    assert!(has_change(&change_set.changes, ChangeKind::Added, "c.cpp"));
    assert!(!has_change(&change_set.changes, ChangeKind::Modified, "b.cpp"));

    assert_eq!(change_set.dirty_paths.len(), 2);
    assert!(change_set.dirty_paths.iter().any(|p| p.ends_with("a.cpp")));
    assert!(change_set.dirty_paths.iter().any(|p| p.ends_with("c.cpp")));

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn git_collect_changes_between_two_fixed_commits_reports_right_set_and_leaves_dirty_empty() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }

    let repo = fresh_repo_dir("fixed_revs");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);
    run_checked(&["git", "config", "user.email", "symhow-test@example.com"], &repo_str);
    run_checked(&["git", "config", "user.name", "symhow-test"], &repo_str);

    write_file(&repo.join("a.cpp"), "int a();\n");
    run_checked(&["git", "add", "-A"], &repo_str);
    run_checked(&["git", "commit", "-q", "-m", "first"], &repo_str);
    let first_sha = rev_parse(&repo_str, "HEAD");
    assert!(first_sha.is_some());

    write_file(&repo.join("a.cpp"), "int a(); // changed\n");
    write_file(&repo.join("d.cpp"), "int d();\n");
    run_checked(&["git", "add", "-A"], &repo_str);
    run_checked(&["git", "commit", "-q", "-m", "second"], &repo_str);
    let second_sha = rev_parse(&repo_str, "HEAD");
    assert!(second_sha.is_some());

    let change_set: ChangeSet =
        collect_changes(&repo_str, &first_sha.unwrap(), second_sha.as_deref()).expect("collect_changes failed");
    assert!(change_set.dirty_paths.is_empty());
    assert!(has_change(&change_set.changes, ChangeKind::Modified, "a.cpp"));
    assert!(has_change(&change_set.changes, ChangeKind::Added, "d.cpp"));

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn git_collect_changes_fails_cleanly_on_a_bad_baseline_rev() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }

    let repo = fresh_repo_dir("bad_baseline");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);
    run_checked(&["git", "config", "user.email", "symhow-test@example.com"], &repo_str);
    run_checked(&["git", "config", "user.name", "symhow-test"], &repo_str);
    write_file(&repo.join("a.cpp"), "int a();\n");
    run_checked(&["git", "add", "-A"], &repo_str);
    run_checked(&["git", "commit", "-q", "-m", "init"], &repo_str);

    let result = collect_changes(&repo_str, "not-a-real-rev", None);
    assert!(result.is_err());
    assert!(!result.unwrap_err().is_empty());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn git_collect_changes_refuses_a_repo_with_unmerged_conflicted_paths() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }

    let repo = fresh_repo_dir("unmerged");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);
    run_checked(&["git", "config", "user.email", "symhow-test@example.com"], &repo_str);
    run_checked(&["git", "config", "user.name", "symhow-test"], &repo_str);

    write_file(&repo.join("f.txt"), "base\n");
    run_checked(&["git", "add", "-A"], &repo_str);
    run_checked(&["git", "commit", "-q", "-m", "base"], &repo_str);
    let base_sha = rev_parse(&repo_str, "HEAD");
    assert!(base_sha.is_some());

    run_checked(&["git", "checkout", "-q", "-b", "b1"], &repo_str);
    write_file(&repo.join("f.txt"), "b1\n");
    run_checked(&["git", "commit", "-q", "-am", "b1"], &repo_str);

    run_checked(&["git", "checkout", "-q", "-"], &repo_str);
    write_file(&repo.join("f.txt"), "main\n");
    run_checked(&["git", "commit", "-q", "-am", "main"], &repo_str);

    // Intentionally not checking the exit code -- a merge conflict makes
    // `git merge` exit non-zero on purpose.
    let merge_argv: Vec<String> = vec!["git".into(), "merge".into(), "-q".into(), "b1".into()];
    let _ = run(&merge_argv, Some(&repo_str));

    let result = collect_changes(&repo_str, &base_sha.unwrap(), None);
    assert!(result.is_err());
    assert!(!result.unwrap_err().is_empty());

    // Clean up the conflicted merge state explicitly for clarity.
    let abort_argv: Vec<String> = vec!["git".into(), "merge".into(), "--abort".into()];
    let _ = run(&abort_argv, Some(&repo_str));
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn git_submodule_exclude_globs_reads_gitmodules_without_initialized_submodules() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    let repo = fresh_repo_dir("gitmodules");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);

    assert!(submodule_paths(&repo_str).is_empty());

    write_file(
        &repo.join(".gitmodules"),
        "[submodule \"duckdb\"]\n\tpath = duckdb\n\turl = https://example.invalid/duckdb.git\n\
         [submodule \"nested\"]\n\tpath = experimental/deps/nested lib\n\turl = https://example.invalid/nested.git\n",
    );

    let globs = submodule_exclude_globs(&repo_str);
    assert_eq!(globs.len(), 2);
    let root = symhow::common::paths::lexical_normalize(&repo_str);
    assert_eq!(globs[0], format!("{}/duckdb/**", root));
    assert_eq!(globs[1], format!("{}/experimental/deps/nested lib/**", root));

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn git_top_level_resolves_the_repo_root() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }

    let repo = fresh_repo_dir("top_level");
    let repo_str = repo.to_string_lossy().into_owned();
    run_checked(&["git", "init", "-q"], &repo_str);
    let top = top_level(&repo_str);
    assert!(top.is_some());
    // weakly_canonical() on both sides: resolves symlinks (e.g. /tmp ->
    // /private/tmp on macOS) before comparing.
    let top_canon = std::fs::canonicalize(top.unwrap()).unwrap();
    let repo_canon = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(top_canon, repo_canon);

    let _ = std::fs::remove_dir_all(&repo);
}
