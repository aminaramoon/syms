//! Incremental index update (hash scan or git diff).
//! CONTRACT: see SPEC.md `update.rs`. Owner: storage agent.
//!
//! Behavior decisions (documented per the task):
//! - git-diff mode unions several `git diff` outputs (worktree-vs-base,
//!   index-vs-HEAD, and untracked files when the ref is `LAST`) into a
//!   candidate set; `--diff-filter=ACMR` excludes deletions there, so deleted
//!   files are instead detected as *previously indexed paths that no longer
//!   exist on disk* (no glob filtering applied to deletions — anything the DB
//!   knows about that vanished is dropped).
//! - `-z` is used on all git listing commands so paths with special characters
//!   are returned unquoted and NUL-separated.
//! - Candidates whose blake3 hash matches the stored hash are skipped, so the
//!   over-inclusive candidate union is safe.
//! - A changed file whose parse fails is warned about on stderr and still gets
//!   its file row refreshed with the *new* hash and an empty symbol set, so it
//!   is not re-detected (and re-failed) on every run. A file that cannot be
//!   *read* at apply time is skipped with a warning and left untouched.
//! - Non-dry runs refresh the `head` meta with `git rev-parse HEAD` whenever
//!   HEAD resolves (skipped silently outside a git repo).
//! - `jobs` is accepted but unused here: update hashes/parses serially; only
//!   the full indexer is parallel.

use crate::lang::LangRegistry;
use crate::store::{db_path, Store};
use crate::types::{FileRecord, Lang};
use crate::walk::{glob_match, walk, Filter};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct UpdateOptions {
    pub path: PathBuf,
    pub git_diff: Option<String>,
    pub dry_run: bool,
    pub langs: BTreeSet<crate::types::Lang>,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub db: Option<PathBuf>,
    pub jobs: Option<usize>,
}

/// What one apply pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSummary {
    pub added: usize,
    pub modified: usize,
    pub removed: usize,
}

pub fn run_update(opts: UpdateOptions) -> Result<()> {
    // Hashing/parsing here is serial; `jobs` only drives the parallel indexer.
    let _ = opts.jobs;

    let root = opts
        .path
        .canonicalize()
        .with_context(|| format!("resolve repo root {}", opts.path.display()))?;
    let db_file = db_path(&root, &opts.db);
    // create=false: a missing database is the user's cue to run `symwhere index`
    // (Store::open emits exactly that message).
    let mut store = Store::open(&db_file, false)?;
    ensure_same_root(&store, &root)?;

    let filter = Filter {
        excludes: opts.excludes.clone(),
        includes: opts.includes.clone(),
        langs: opts.langs.clone(),
    };
    let registry = LangRegistry::new();

    let (changed, removed) = match opts.git_diff.as_deref() {
        Some(git_ref) => {
            let (candidates, removed) = git_diff_changes(&root, git_ref, &store, &filter)?;
            (classify_against(&store, &root, &candidates)?, removed)
        }
        None => scan_changes(&root, &filter, &store)?,
    };

    if opts.dry_run {
        print_plan(&store, &root, &changed, &removed, &registry)?;
        return Ok(());
    }

    let summary = apply_changes(&mut store, &root, &changed, &removed, &registry)?;

    // Record HEAD whenever it resolves, so a later `update --git-diff`
    // (or `--git-diff LAST`) diffs against a fresh base.
    if let Some(head) = git_head(&root) {
        store.set_meta("head", &head)?;
    }

    let total = store.count_symbols()?;
    println!(
        "updated: {} added, {} modified, {} removed files; symbols now {}",
        summary.added, summary.modified, summary.removed, total
    );
    Ok(())
}

/// Guard against updating the wrong repo with an existing index: both change
/// detectors can *remove* rows, so refuse to run when the index clearly belongs
/// to a different root. Only enforced when the indexer recorded a `root` meta
/// key; absence means "cannot check" and the update proceeds.
fn ensure_same_root(store: &Store, root: &Path) -> Result<()> {
    let Some(stored) = store.get_meta("root")? else {
        return Ok(());
    };
    let stored_path = PathBuf::from(&stored);
    let same = stored_path == root
        || stored_path
            .canonicalize()
            .map(|p| p == root)
            .unwrap_or(false);
    if !same {
        bail!(
            "index at this database was built for {} but updating {}; run a full `symwhere index` first",
            stored,
            root.display()
        );
    }
    Ok(())
}

/// Pending changes: files to (re)index with their language, and paths to drop.
type Changes = (Vec<(String, Lang)>, Vec<String>);

/// git-diff change detection. `ref` is either `LAST` (diff against the HEAD
/// commit stored in the index at last index/update time) or an explicit
/// revision such as `HEAD~1`.
fn git_diff_changes(root: &Path, git_ref: &str, store: &Store, filter: &Filter) -> Result<Changes> {
    let last_mode = git_ref == "LAST";
    let base = if last_mode {
        store.get_meta("head")?.ok_or_else(|| {
            anyhow::anyhow!("index has no stored HEAD commit; run a full `symwhere index` first")
        })?
    } else {
        git_ref.to_string()
    };

    // Candidate union: `git diff <base>` already covers staged+unstaged
    // changes relative to `base`; `--cached` adds index-vs-HEAD moves; in
    // LAST mode untracked files are added too. Over-inclusion is fine
    // because unchanged candidates are dropped by hash comparison later.
    let mut rels: BTreeSet<String> = BTreeSet::new();
    rels.extend(git_lines(
        root,
        &["diff", "--name-only", "-z", "--diff-filter=ACMR", &base],
    )?);
    rels.extend(git_lines(
        root,
        &[
            "diff",
            "--cached",
            "--name-only",
            "-z",
            "--diff-filter=ACMR",
        ],
    )?);
    if last_mode {
        rels.extend(git_lines(
            root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);
    }

    let changed = filter_candidates(rels, filter);

    // Deleted files: indexed paths that vanished from disk (ACMR hides
    // deletions from the diff filters above).
    let removed: Vec<String> = store
        .file_hashes()?
        .into_keys()
        .filter(|rel| !root.join(rel).exists())
        .collect();

    Ok((changed, removed))
}

/// Hash-scan change detection: walk the repo with the filter, hash every
/// candidate, and compare against the DB. New/changed files come back as
/// `changed`; indexed paths the walk no longer sees come back as `removed`.
fn scan_changes(root: &Path, filter: &Filter, store: &Store) -> Result<Changes> {
    let known = store.file_hashes()?;
    let mut changed = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for abs in walk(root, filter) {
        // A path we cannot make relative must abort the update: silently
        // skipping it would make it look "missing" and delete its rows.
        let rel = abs
            .strip_prefix(root)
            .with_context(|| format!("walk returned path outside root: {}", abs.display()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let lang = match Lang::detect(rel) {
            Some(lang) => lang,
            None => continue,
        };
        // Re-assert the filter independently of walk's exact semantics.
        if !filter.langs.contains(&lang) || !path_allowed(&rel_str, filter) {
            continue;
        }
        let hash = hash_file(&abs)?;
        seen.insert(rel_str.clone());
        if known.get(&rel_str) != Some(&hash) {
            changed.push((rel_str, lang));
        }
    }

    let removed = known.into_keys().filter(|p| !seen.contains(p)).collect();
    Ok((changed, removed))
}

/// Keep only candidates whose current blake3 hash differs from the stored
/// hash (or that are not in the DB at all). Used by git-diff mode, where the
/// candidate union is deliberately over-inclusive.
fn classify_against(
    store: &Store,
    root: &Path,
    candidates: &[(String, Lang)],
) -> Result<Vec<(String, Lang)>> {
    let known = store.file_hashes()?;
    let mut changed = Vec::new();
    for (rel, lang) in candidates {
        let hash = hash_file(&root.join(rel))?;
        if known.get(rel) != Some(&hash) {
            changed.push((rel.clone(), *lang));
        }
    }
    Ok(changed)
}

/// Drop candidates that the language set or include/exclude globs reject.
fn filter_candidates(rels: BTreeSet<String>, filter: &Filter) -> Vec<(String, Lang)> {
    rels.into_iter()
        .filter_map(|rel| {
            let lang = Lang::detect(Path::new(&rel))?;
            if !filter.langs.contains(&lang) {
                return None;
            }
            if !path_allowed(&rel, filter) {
                return None;
            }
            Some((rel, lang))
        })
        .collect()
}

/// Include/exclude glob rules for one repo-relative path: if includes are
/// non-empty the path must match at least one; any exclude match rejects it.
fn path_allowed(rel: &str, filter: &Filter) -> bool {
    if !filter.includes.is_empty() && !filter.includes.iter().any(|p| glob_match(p, rel)) {
        return false;
    }
    if filter.excludes.iter().any(|p| glob_match(p, rel)) {
        return false;
    }
    true
}

/// Core, testable apply step (no `Store` construction inside): re-parse every
/// changed file, replace its symbol rows, and remove deleted files.
/// Returns how many files were added / modified / removed.
pub fn apply_changes(
    store: &mut Store,
    root: &Path,
    changed: &[(String, Lang)],
    removed: &[String],
    registry: &LangRegistry,
) -> Result<UpdateSummary> {
    let mut summary = UpdateSummary::default();
    let known: HashMap<String, String> = store.file_hashes()?;

    for (rel, lang) in changed {
        let abs = root.join(rel);
        let src = match std::fs::read(&abs) {
            Ok(src) => src,
            Err(err) => {
                // Cannot hash or parse it; keep the previous DB state and warn.
                eprintln!("warn: skipping {rel}: {err}");
                continue;
            }
        };
        let syms = match crate::index::parse_file(&abs, *lang, &src, registry) {
            Ok(syms) => syms,
            Err(err) => {
                // Refresh the row with the new hash but no symbols so the
                // file is not re-detected and re-failed on every run.
                eprintln!("warn: parse failed for {rel}: {err:#}; storing no symbols");
                Vec::new()
            }
        };
        let rec = FileRecord {
            path: rel.clone(),
            lang: *lang,
            hash: blake3::hash(&src).to_string(),
            size: src.len() as u64,
        };
        store.upsert_file_with_symbols(&rec, &syms)?;
        if known.contains_key(rel) {
            summary.modified += 1;
        } else {
            summary.added += 1;
        }
    }

    for rel in removed {
        store.remove_file(rel)?;
        summary.removed += 1;
    }

    Ok(summary)
}

/// Dry-run report: per-file planned action with symbol deltas, nothing written.
/// - `A path (+n)`        — new file with n symbols
/// - `M path (+a -b symbols)` — modified; a = symbols gained, b = lost
/// - `D path (-n)`        — deleted file with n indexed symbols
fn print_plan(
    store: &Store,
    root: &Path,
    changed: &[(String, Lang)],
    removed: &[String],
    registry: &LangRegistry,
) -> Result<()> {
    let known = store.file_hashes()?;
    let mut any = false;

    for (rel, lang) in changed {
        any = true;
        let new_count = new_symbol_count(root, rel, *lang, registry);
        if known.contains_key(rel) {
            let old_count = store.count_symbols_for_file(rel)?;
            let old_count = usize::try_from(old_count).unwrap_or(usize::MAX);
            let plus = new_count.saturating_sub(old_count);
            let minus = old_count.saturating_sub(new_count);
            println!("M {rel} (+{plus} -{minus} symbols)");
        } else {
            println!("A {rel} (+{new_count})");
        }
    }

    for rel in removed {
        any = true;
        let old_count = store.count_symbols_for_file(rel)?;
        let old_count = usize::try_from(old_count).unwrap_or(usize::MAX);
        println!("D {rel} (-{old_count})");
    }

    if !any {
        println!("up to date: no changes detected");
    }
    Ok(())
}

/// Parse a file purely to count its symbols (dry-run); failures warn and
/// report 0, mirroring what a real apply would store.
fn new_symbol_count(root: &Path, rel: &str, lang: Lang, registry: &LangRegistry) -> usize {
    let abs = root.join(rel);
    let src = match std::fs::read(&abs) {
        Ok(src) => src,
        Err(err) => {
            eprintln!("warn: cannot read {rel}: {err}");
            return 0;
        }
    };
    match crate::index::parse_file(&abs, lang, &src, registry) {
        Ok(syms) => syms.len(),
        Err(err) => {
            eprintln!("warn: parse failed for {rel}: {err:#}");
            0
        }
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(blake3::hash(&data).to_string())
}

/// Run `git -C root <args>` and return NUL-separated, unquoted output lines.
fn git_lines(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("spawn git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// `git rev-parse HEAD` as an Option: None when not a repo or HEAD is unborn.
fn git_head(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if head.is_empty() {
            None
        } else {
            Some(head)
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "symwhere-update-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize so strip_prefix works regardless of how walk() builds
        // its paths (macOS /tmp symlinks, etc.).
        dir.canonicalize().unwrap()
    }

    fn empty_filter() -> Filter {
        Filter {
            excludes: vec![],
            includes: vec![],
            langs: Lang::ALL.into_iter().collect(),
        }
    }

    #[test]
    fn apply_adds_modifies_and_removes() {
        let dir = tempdir("apply");
        std::fs::write(dir.join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn beta() {}\n").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let registry = LangRegistry::new();

        let both = vec![
            ("a.rs".to_string(), Lang::Rust),
            ("b.rs".to_string(), Lang::Rust),
        ];

        // First pass: both files are new.
        let summary = apply_changes(&mut store, &dir, &both, &[], &registry).unwrap();
        assert_eq!(
            summary,
            UpdateSummary {
                added: 2,
                modified: 0,
                removed: 0
            }
        );
        let hashes = store.file_hashes().unwrap();
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains_key("a.rs") && hashes.contains_key("b.rs"));

        // Change one file; only it should be re-detected.
        std::fs::write(dir.join("b.rs"), "fn beta2() {}\nfn gamma() {}\n").unwrap();
        let (chg, removed) = scan_changes(&dir, &empty_filter(), &store).unwrap();
        assert_eq!(chg, vec![("b.rs".to_string(), Lang::Rust)]);
        assert!(removed.is_empty());

        // Apply the change and drop the other file entirely.
        let summary =
            apply_changes(&mut store, &dir, &chg, &["a.rs".to_string()], &registry).unwrap();
        assert_eq!(
            summary,
            UpdateSummary {
                added: 0,
                modified: 1,
                removed: 1
            }
        );
        let hashes = store.file_hashes().unwrap();
        assert_eq!(hashes.len(), 1);
        assert!(!hashes.contains_key("a.rs"));
        assert!(store.get_meta("head").unwrap().is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unchanged_candidates_are_filtered_by_hash() {
        let dir = tempdir("classify");
        std::fs::write(dir.join("a.rs"), "fn alpha() {}\n").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let registry = LangRegistry::new();
        let candidates = vec![("a.rs".to_string(), Lang::Rust)];
        apply_changes(&mut store, &dir, &candidates, &[], &registry).unwrap();

        // Same content again → hash matches → nothing to do.
        let fresh = classify_against(&store, &dir, &candidates).unwrap();
        assert!(fresh.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dry_run_plan_writes_nothing() {
        let dir = tempdir("plan");
        std::fs::write(dir.join("a.rs"), "fn alpha() {}\n").unwrap();

        let store = Store::open_in_memory().unwrap();
        let registry = LangRegistry::new();
        let changed = vec![("a.rs".to_string(), Lang::Rust)];
        let removed = vec!["gone.rs".to_string()];

        print_plan(&store, &dir, &changed, &removed, &registry).unwrap();

        // Nothing was written: still empty, and the unknown "gone.rs" lookup
        // did not error.
        assert!(store.file_hashes().unwrap().is_empty());
        assert_eq!(store.count_symbols().unwrap(), 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
