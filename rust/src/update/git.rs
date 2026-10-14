//! Thin wrappers over `common::process::run()` for the handful of `git`
//! invocations `symhow update` needs. Mirrors `symhow::update::Git`.

use std::collections::BTreeSet;

use crate::common::paths::lexical_normalize;
use crate::common::process::run;

use super::git_diff::{parse_name_status_z, parse_status_porcelain_z, status_has_unmerged_entries, ChangeKind, ChangedFile};

fn trim_trailing_newline(mut s: String) -> String {
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

fn run_git(args: &[&str]) -> crate::common::process::ProcessResult {
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    run(&argv, None)
}

/// Resolves `rev` to its full 40-hex commit SHA. `None` if git isn't on
/// PATH, `repo_root` isn't inside a work tree, or `rev` doesn't resolve.
pub fn rev_parse(repo_root: &str, rev: &str) -> Option<String> {
    let spec = format!("{}^{{commit}}", rev);
    let result = run_git(&["git", "-C", repo_root, "rev-parse", "--verify", &spec]);
    if result.exit_code != 0 {
        return None;
    }
    Some(trim_trailing_newline(result.out))
}

/// Resolves the git work tree root containing `any_dir`.
pub fn top_level(any_dir: &str) -> Option<String> {
    let result = run_git(&["git", "-C", any_dir, "rev-parse", "--show-toplevel"]);
    if result.exit_code != 0 {
        return None;
    }
    Some(lexical_normalize(&trim_trailing_newline(result.out)))
}

/// Runs `git -C repoRoot diff --name-status -z --find-renames ...` and
/// returns its raw (NUL-separated) stdout. If `to` is `None`, diffs
/// `baseline` against the working tree; else `baseline..to`.
pub fn diff_name_status_z(repo_root: &str, baseline: &str, to: Option<&str>) -> Option<String> {
    let range = match to {
        Some(to) => format!("{}..{}", baseline, to),
        None => baseline.to_string(),
    };
    let result = run_git(&["git", "-C", repo_root, "diff", "--name-status", "-z", "--find-renames", &range]);
    if result.exit_code != 0 {
        return None;
    }
    Some(result.out)
}

/// Runs `git -C repoRoot status --porcelain -z --untracked-files=all`.
pub fn status_porcelain_z(repo_root: &str) -> Option<String> {
    let result = run_git(&["git", "-C", repo_root, "status", "--porcelain", "-z", "--untracked-files=all"]);
    if result.exit_code != 0 {
        return None;
    }
    Some(result.out)
}

/// Absolute, lexically-normal paths of every submodule declared in
/// `<repo_root>/.gitmodules`. Empty if there is no .gitmodules.
pub fn submodule_paths(repo_root: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let result = run_git(&[
        "git",
        "-C",
        repo_root,
        "config",
        "--file",
        ".gitmodules",
        "--get-regexp",
        r"^submodule\..*\.path$",
    ]);
    if result.exit_code != 0 {
        // exit 1 = no .gitmodules / no matches
        return paths;
    }
    for line in result.out.split('\n') {
        // "submodule.<name>.path <value>" -- the value may contain spaces.
        let Some(sep) = line.find(' ') else { continue };
        if sep + 1 >= line.len() {
            continue;
        }
        let abs = std::path::Path::new(repo_root).join(&line[sep + 1..]);
        paths.push(lexical_normalize(&abs.to_string_lossy()));
    }
    paths
}

/// `submodule_paths()` turned into symhow exclude globs (`<abs path>/**`).
pub fn submodule_exclude_globs(repo_root: &str) -> Vec<String> {
    submodule_paths(repo_root).into_iter().map(|p| format!("{p}/**")).collect()
}

/// The combined result of `collect_changes()`.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    pub changes: Vec<ChangedFile>,
    pub dirty_paths: Vec<String>,
}

/// Computes the full changed-file set between `baseline` and either the
/// working tree (`to == None`) or a second fixed rev. See the C++ header for
/// the full rationale (untracked handling, unmerged refusal). Returns
/// `Err(message)` where the C++ returns nullopt with `*err` set.
pub fn collect_changes(
    repo_root: &str,
    baseline: &str,
    to: Option<&str>,
) -> Result<ChangeSet, String> {
    let Some(_baseline_sha) = rev_parse(repo_root, baseline) else {
        return Err(format!("couldn't resolve baseline rev '{}' in repo '{}'", baseline, repo_root));
    };
    if let Some(to) = to {
        if rev_parse(repo_root, to).is_none() {
            return Err(format!("couldn't resolve 'to' rev '{}' in repo '{}'", to, repo_root));
        }
    }

    let Some(diff_out) = diff_name_status_z(repo_root, baseline, to) else {
        return Err(format!("'git diff --name-status' failed in repo '{}'", repo_root));
    };

    let mut change_set = ChangeSet {
        changes: parse_name_status_z(&diff_out, repo_root),
        dirty_paths: Vec::new(),
    };

    for c in &change_set.changes {
        if c.kind == ChangeKind::Unmerged {
            return Err(format!(
                "repository has unmerged paths; resolve conflicts first ('{}' is unmerged)",
                c.new_path
            ));
        }
    }

    if to.is_none() {
        let Some(status_out) = status_porcelain_z(repo_root) else {
            return Err(format!("'git status --porcelain' failed in repo '{}'", repo_root));
        };

        if status_has_unmerged_entries(&status_out) {
            return Err("repository has unmerged paths; resolve conflicts first".to_string());
        }

        change_set.dirty_paths = parse_status_porcelain_z(&status_out, repo_root);

        // Dedup untracked-as-Added entries against whatever `git diff`
        // already reported.
        let mut already_changed: BTreeSet<String> = BTreeSet::new();
        for c in &change_set.changes {
            if !c.new_path.is_empty() {
                already_changed.insert(c.new_path.clone());
            }
            if !c.old_path.is_empty() {
                already_changed.insert(c.old_path.clone());
            }
        }

        // Re-derive untracked ("??") entries from the raw output.
        let raw = status_out.as_str();
        let mut start = 0usize;
        while start < raw.len() {
            let entry_end = raw[start..].find('\0').map(|r| start + r).unwrap_or(raw.len());
            let entry = &raw[start..entry_end];
            let next = (entry_end + 1).min(raw.len());
            if entry.len() > 3 && entry.starts_with("??") {
                let abs = std::path::Path::new(repo_root).join(&entry[3..]);
                let path = lexical_normalize(&abs.to_string_lossy());
                if already_changed.insert(path.clone()) {
                    change_set.changes.push(ChangedFile { kind: ChangeKind::Added, old_path: String::new(), new_path: path });
                }
            }
            start = next;
        }
    }

    Ok(change_set)
}