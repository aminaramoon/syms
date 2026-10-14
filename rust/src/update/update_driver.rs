//! Phase 3: `symhow update` orchestration -- reads a database's stored
//! provenance, works out which files changed since the last index/update
//! (via git or an explicit file list), clears the affected facts
//! (`super::invalidator`) and re-indexes only what's necessary
//! (`crate::index::index_driver::run_index` with preset_entries), all
//! against a temp copy that's atomically renamed over the original on
//! success (or, with --in-place, directly against the real file with a
//! recovery flag set for the duration). Mirrors
//! `symhow::update::UpdateDriver` (src/update/UpdateDriver.cpp); every
//! meta key, SQL statement and message string is preserved byte-for-byte.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use crate::common::content_hash::file_content_hash;
use crate::common::paths::{lexical_normalize, NormalizationCache};
use crate::common::{Error, Result};
use crate::db::meta::{delete_meta, get_meta, set_meta};
use crate::db::{Database, WritableDb};
use crate::index::arg_sanitizer::SanitizerOptions;
use crate::index::compile_db::{load, path_args_hashes, CompileDbOptions, CompileEntry};
use crate::index::cuda_support::CudaMode;
use crate::index::file_registry::SystemFileRows;
use crate::index::index_driver::{run_index, IndexRunOptions};
use crate::index::provenance::{read_provenance, write_provenance, Provenance, K_WALKER_VERSION};
use crate::model::IndexSummary;
use crate::notes::sync::sync_notes;
use crate::update::file_matcher::{match_changed_with, select_entries_with, MatchInput};
use crate::update::git::{collect_changes, rev_parse, status_porcelain_z, submodule_exclude_globs};
use crate::update::git_diff::{parse_status_porcelain_z, ChangeKind, ChangedFile};
use crate::update::invalidator::{compute_clear_set, invalidate, sweep_orphans, ClearPlan, GcStats};

fn millis_since(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// "%Y-%m-%dT%H:%M:%SZ" UTC timestamp, like the C++ strftime/gmtime_r pair.
fn utc_timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Best-effort: returns the current dirty-paths list (mirrors the index
/// driver's provenance-building behavior), or the previous value if git
/// status can't be run.
fn current_dirty_paths(repo_root: &str, fallback: &[String]) -> Vec<String> {
    if repo_root.is_empty() {
        return fallback.to_vec();
    }
    match status_porcelain_z(repo_root) {
        Some(status) => parse_status_porcelain_z(&status, repo_root),
        None => fallback.to_vec(),
    }
}

/// `std::filesystem::absolute(path).lexically_normal().string()`: prepend
/// the current working directory (no symlink resolution), then normalize
/// `.`/`..` textually.
fn absolute_lexically_normal(path: &str) -> String {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(_) => p.to_path_buf(),
        }
    };
    lexical_normalize(&abs.to_string_lossy())
}

// Builds the `symhow.update-summary/1` envelope for the "error" case ("--json"
// still needs to emit SOMETHING structured on refusals/hard errors, not
// just silence). Never used for a successful run (see build_summary_json()
// below for that).
fn build_error_json(options: &UpdateRunOptions, message: &str, exit_code: i32) -> serde_json::Value {
    serde_json::json!({
        "symhow_version": &options.symhow_version,
        "schema": "symhow.update-summary/1",
        "database": &options.db_path,
        "error": message,
        "exit_code": exit_code,
    })
}

fn report_refused(options: &UpdateRunOptions, message: &str) {
    eprintln!("symhow update: refused: {}", message);
    if options.json {
        println!("{}", build_error_json(options, message, 2));
    }
}

fn report_error(options: &UpdateRunOptions, message: &str) {
    eprintln!("symhow update: error: {}", message);
    if options.json {
        println!("{}", build_error_json(options, message, 1));
    }
}

// Builds the full `symhow.update-summary/1` envelope for a successful (rc 0 or
// 5) run, dry-run included, straight from `summary`'s already-populated
// fields -- shared by the "nothing to do" (exit 5), --dry-run (exit 0), and
// real-update (exit 0) JSON output paths so all three stay in sync.
fn build_summary_json(options: &UpdateRunOptions, s: &UpdateSummary) -> serde_json::Value {
    let mut j = serde_json::json!({
        "symhow_version": &options.symhow_version,
        "schema": "symhow.update-summary/1",
        "database": &s.database,
        "dry_run": s.dry_run,
        "in_place": s.in_place,
        "repo": {
            "root": &s.repo_root,
            "from": &s.from_rev,
            "to": &s.to_rev,
        },
        "files": {
            "changed": s.changed_count,
            "deleted": s.deleted_count,
            "renamed": s.renamed_count,
            "flag_changed": s.flag_changed_count,
            "unmatched": s.unmatched_count,
            "cleared": s.plan.cleared_file_ids.len(),
            "include_closure": s.plan.include_closure_added,
            "headers_pruned": s.plan.headers_pruned,
            "cleared_paths": &s.plan.cleared_paths,
            "unmatched_paths": &s.unmatched_paths,
        },
        "tus": {
            "to_index": s.tus_to_index,
            "new": s.tus_new,
            "retried": s.tus_retried,
            "skipped_missing": s.skipped_missing,
        },
        "gc": {
            "files": s.gc.files,
            "nodes": s.gc.nodes,
            "edges": s.gc.edges,
            "locations": s.gc.locations,
            "local_symbols": s.gc.local_symbols,
            "errors": s.gc.errors,
            "symbols_reset": s.gc.symbols_reset,
            "non_indexed_files": s.gc.non_indexed_files,
            "orphan_elements": s.gc.orphan_elements,
        },
        "elapsed_ms": s.elapsed_ms,
        "index_elapsed_ms": s.index_elapsed_ms,
    });
    j["index"] = if s.index_ran {
        serde_json::json!({
            "tus_total": s.idx_tus_total,
            "tus_attempted": s.idx_tus_attempted,
            "tus_with_content": s.idx_tus_with_content,
            "tus_failed": s.idx_tus_failed,
            "symbols": s.idx_symbols,
            "references": s.idx_references,
            "files": s.idx_files,
            "errors": s.idx_errors,
            "name_collisions": s.idx_name_collisions,
        })
    } else {
        serde_json::json!({})
    };
    j["notes"] = if s.notes_ran {
        serde_json::json!({
            "noted": s.notes_noted,
            "stale": s.notes_stale,
            "newly_stale": s.notes_newly_stale,
            "problems": s.notes_problems,
        })
    } else {
        serde_json::json!({})
    };
    j
}

// RAII guard for `<db>.symhow-lock`, held for the whole run_update() call
// (including the exit-5 "nothing to do" provenance refresh) so two
// concurrent `symhow update` invocations against the same database can't
// race each other. Uses O_CREAT|O_EXCL -- atomic create-if-absent, same
// trick `flock`-less tools have used forever -- rather than a real flock()
// so a stale lock from a crashed process is trivially diagnosable (it's
// just a file) and recoverable (the message below says so).
struct UpdateLock {
    path: String,
    acquired: bool,
}

impl Default for UpdateLock {
    fn default() -> Self {
        UpdateLock { path: String::new(), acquired: false }
    }
}

impl UpdateLock {
    // Returns true (and holds the lock until this object is destroyed, or
    // release() is called) if `db_path + ".symhow-lock"` didn't already
    // exist; Err(with message) if it did.
    fn acquire(&mut self, db_path: &str) -> std::result::Result<(), String> {
        self.path = format!("{}.symhow-lock", db_path);
        // create_new(true) == O_CREAT|O_EXCL: atomic create-if-absent.
        match std::fs::OpenOptions::new().create_new(true).write(true).open(&self.path) {
            Ok(_file) => {
                self.acquired = true;
                Ok(())
            }
            Err(_) => {
                let err = format!(
                    "another symhow update is running (or crashed; remove {} if stale)",
                    self.path
                );
                self.path.clear();
                Err(err)
            }
        }
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = std::fs::remove_file(&self.path);
            self.acquired = false;
        }
    }
}

// Absolute path of `path`'s ".srctrlprj" sidecar, per the vendored writer's
// own `rfind('.')`-on-the-whole-path derivation -- used both to build the
// temp copy's own sidecar name and to delete it afterward.
fn proj_sidecar_for(path: &str) -> String {
    match path.rfind('.') {
        Some(dot) => format!("{}.srctrlprj", &path[..dot]),
        None => format!("{}.srctrlprj", path),
    }
}

fn cleanup_temp(made_temp_copy: bool, work: &str, work_proj: &str) {
    if made_temp_copy {
        let _ = std::fs::remove_file(work);
        let _ = std::fs::remove_file(format!("{}-journal", work));
        let _ = std::fs::remove_file(work_proj);
    }
}

fn table_exists(db: &Database, name: &str) -> bool {
    let mut found = false;
    let _ = db.run("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", [name], |_row| {
        found = true;
        Ok(())
    });
    found
}

/// `static_cast<int>(ChangeKind)` for the change-dedup key -- the C++ enum's
/// declaration order (Added, Modified, Deleted, Renamed, Unmerged).
fn change_kind_int(kind: ChangeKind) -> i32 {
    match kind {
        ChangeKind::Added => 0,
        ChangeKind::Modified => 1,
        ChangeKind::Deleted => 2,
        ChangeKind::Renamed => 3,
        ChangeKind::Unmerged => 4,
    }
}

struct EffectiveOptions {
    db_options: CompileDbOptions,
    sanitizer_options: SanitizerOptions,
    system_file_rows: SystemFileRows,
}

/// Options for a `symhow update` run. Mirrors `symhow::update::UpdateRunOptions`
/// field for field.
#[derive(Debug, Clone)]
pub struct UpdateRunOptions {
    pub db_path: String,
    /// --repo override; empty = use stored symhow_repo_root.
    pub repo_root: String,
    /// hidden --from override; empty = use stored symhow_repo_commit.
    pub from_rev: String,
    /// --to override; empty = "HEAD".
    pub to_rev: String,

    /// -c override; empty = use stored symhow_cdb_path.
    pub cdb_path: String,
    /// --files: bypass git entirely.
    pub explicit_files: Vec<String>,

    // Overrides for the stored options (symhow_options_json). A field is only
    // treated as "given" if the corresponding *_given flag is set -- an
    // empty vector/string CLI override (e.g. --include given zero times)
    // must not be confused with "not given at all".
    pub db_overrides: CompileDbOptions,
    /// true if --remap/--include/--exclude given at all.
    pub db_overrides_given: bool,
    /// append `<submodule>/**` exclude globs from the repo's .gitmodules.
    pub exclude_submodules: bool,
    pub cuda_mode_override: CudaMode,
    pub cuda_mode_given: bool,
    pub system_file_rows_override: SystemFileRows,
    pub system_file_rows_given: bool,
    pub extra_arg_override: Vec<String>,
    pub extra_arg_given: bool,
    pub drop_arg_override: Vec<String>,
    pub drop_arg_given: bool,
    pub std_override: String,
    pub std_given: bool,
    pub c_std_override: String,
    pub c_std_given: bool,

    pub dry_run: bool,
    pub in_place: bool,
    pub no_vacuum: bool,
    pub json: bool,
    pub verbose: i32,
    pub symhow_version: String,

    /// --parallel/-j N for the re-index step (IndexRunOptions::parallel) --
    /// see its doc comment. Default 1 (sequential, unchanged behavior).
    /// Deliberately NOT part of symhow_options_json/Provenance: it doesn't
    /// affect what gets indexed.
    pub parallel: i32,
}

impl Default for UpdateRunOptions {
    fn default() -> Self {
        UpdateRunOptions {
            db_path: String::new(),
            repo_root: String::new(),
            from_rev: String::new(),
            to_rev: String::new(),
            cdb_path: String::new(),
            explicit_files: Vec::new(),
            db_overrides: CompileDbOptions::default(),
            db_overrides_given: false,
            exclude_submodules: false,
            cuda_mode_override: CudaMode::Auto,
            cuda_mode_given: false,
            system_file_rows_override: SystemFileRows::Direct,
            system_file_rows_given: false,
            extra_arg_override: Vec::new(),
            extra_arg_given: false,
            drop_arg_override: Vec::new(),
            drop_arg_given: false,
            std_override: String::new(),
            std_given: false,
            c_std_override: String::new(),
            c_std_given: false,
            dry_run: false,
            in_place: false,
            no_vacuum: false,
            json: false,
            verbose: 0,
            symhow_version: String::new(),
            parallel: 1,
        }
    }
}

/// Result of a `symhow update` run. Mirrors `symhow::update::UpdateSummary`
/// field for field, including `plan: ClearPlan` and `gc: GcStats`.
#[derive(Debug, Clone, Default)]
pub struct UpdateSummary {
    pub database: String,
    pub dry_run: bool,
    pub in_place: bool,

    pub repo_root: String,
    pub from_rev: String,
    pub to_rev: String,

    pub changed_count: u32,
    pub deleted_count: u32,
    pub renamed_count: u32,
    pub flag_changed_count: u32,
    pub unmatched_count: u32,
    pub unmatched_paths: Vec<String>,

    /// cleared_paths/cleared_file_ids/seed_files/include_closure_added/
    /// headers_pruned.
    pub plan: ClearPlan,

    pub tus_to_index: u32,
    pub tus_new: u32,
    pub tus_retried: u32,
    pub skipped_missing: u32,

    pub gc: GcStats,

    // Populated (if indexing ran) from `crate::model::IndexSummary`, via
    // IndexRunOptions::json_summary=false + a direct summary_out pointer --
    // mirrors the index summary's "counts" object.
    pub idx_tus_total: u32,
    pub idx_tus_attempted: u32,
    pub idx_tus_with_content: u32,
    pub idx_tus_failed: u32,
    pub idx_symbols: u32,
    pub idx_references: u32,
    pub idx_files: u32,
    pub idx_errors: u32,
    pub idx_name_collisions: u32,
    pub index_ran: bool,

    pub elapsed_ms: u64,
    pub index_elapsed_ms: u64,

    pub nothing_to_do: bool,

    // Phase 3 of docs/NOTES_DESIGN.md: post-update `sync_notes()` results,
    // populated (notes_ran = true) only when a notes dir was known
    // (meta.symhow_notes_dir, or <repo root>/notes if it exists) AND
    // actually exists on disk. Never fails the update: a notes-sync error is
    // caught and reported (notes_ran stays false), never surfaced as a
    // non-zero exit code. `notes_newly_stale` counts serialized_names that
    // were NOT stale in `symhow_note` before this update's re-sync but ARE
    // after it (comparing against whatever the previous `symhow notes sync`
    // left behind -- the symhow_note* cache is untouched by re-indexing
    // itself).
    pub notes_ran: bool,
    pub notes_noted: u32,
    pub notes_stale: u32,
    pub notes_newly_stale: u32,
    pub notes_problems: u32,
}

/// One row of the target database's `symhow_tu` table, as read up front.
#[derive(Clone)]
struct TuRow {
    path: String,
    args_hash: String,
    status: String,
}

fn resolve_db_path(p: &str, db_path_by_norm: &HashMap<String, String>, cache: &mut NormalizationCache) -> String {
    match db_path_by_norm.get(&cache.resolve(p)) {
        Some(resolved) => resolved.clone(),
        None => p.to_string(),
    }
}

/// Exit codes: 0 ok (including --dry-run / nothing-to-do), 1 hard error,
/// 2 refused (legacy/no-provenance db, update already in progress, bad
/// options), 5 nothing to do.
pub fn run_update(options: &UpdateRunOptions, out: Option<&mut UpdateSummary>) -> i32 {
    let run_start = Instant::now();
    // One cache shared by planning, matching and selection; never across runs.
    let mut normalize_path = NormalizationCache::default();
    let mut summary = UpdateSummary {
        database: options.db_path.clone(),
        dry_run: options.dry_run,
        in_place: options.in_place,
        ..UpdateSummary::default()
    };

    // --- 0. concurrency guard + hot-journal check -----------------------------
    // Held for the ENTIRE function (RAII -- released on every return path,
    // including every early refusal/error below and the exit-5 "nothing to
    // do" path), so two concurrent `symhow update` runs against the same
    // database can't race each other (e.g. both deciding it's safe to set
    // symhow_update_in_progress, or both copying the same working-copy name).
    let mut lock = UpdateLock::default();
    if let Err(lock_err) = lock.acquire(&options.db_path) {
        report_refused(options, &lock_err);
        return 2;
    }

    // A `-journal`/`-wal` file next to the original means a previous
    // process (sqlite itself, or a previous `symhow update`/`symhow index`)
    // may have crashed mid-write -- copying (or, for --in-place, writing
    // directly into) a database with a hot journal risks propagating an
    // inconsistent snapshot. Refuse rather than guess.
    if Path::new(&format!("{}-journal", options.db_path)).exists()
        || Path::new(&format!("{}-wal", options.db_path)).exists()
    {
        report_refused(
            options,
            &format!(
                "found a hot journal/WAL file next to '{}' -- a previous process may have crashed mid-write; verify the database's integrity (e.g. `sqlite3 {} 'PRAGMA integrity_check;'`) before retrying",
                options.db_path, options.db_path
            ),
        );
        return 2;
    }

    // --- 1. open read-only, read provenance, run refusal checks ------------
    let mut resuming_interrupted_index = false;
    let prov_opt: Option<Provenance>;
    {
        let db = match Database::open(options.db_path.as_str(), options.verbose) {
            Ok(db) => db,
            Err(e) => {
                report_error(options, &e.0);
                return 1;
            }
        };
        let p = read_provenance(db.conn());
        if p.is_some() {
            if get_meta(db.conn(), "symhow_update_in_progress").as_deref() == Some("1") {
                report_refused(
                    options,
                    "an update is already in progress on this database (symhow_update_in_progress is set) -- if a \
                     previous `symhow update --in-place` was interrupted, re-index from scratch to recover",
                );
                return 2;
            }

            // Phase 8: a database left with symhow_index_in_progress="1" is an
            // INTERRUPTED `symhow index` run, not a corrupt/mid-write one (its
            // provenance and every completed checkpoint's facts are
            // durable) -- resume it instead of refusing. See the retry-set
            // computation below (step "Phase 8 resume") for how the
            // never-indexed suffix gets picked up.
            if get_meta(db.conn(), "symhow_index_in_progress").as_deref() == Some("1") {
                resuming_interrupted_index = true;
                if !options.json {
                    eprintln!("symhow update: resuming interrupted index (symhow_index_in_progress was set)");
                }
            }
        }
        prov_opt = p;
    }

    let Some(prov) = prov_opt else {
        report_refused(
            options,
            "no provenance found (not written by this version of symhow, or by the real Sourcetrail indexer)",
        );
        return 2;
    };
    if prov.walker_version != K_WALKER_VERSION {
        report_refused(
            options,
            &format!(
                "database was written by walker version {}, this symhow is walker version {} -- re-index from scratch",
                prov.walker_version, K_WALKER_VERSION
            ),
        );
        return 2;
    }
    if prov.system_file_rows == SystemFileRows::All {
        report_refused(
            options,
            "database was indexed with --system-file-rows=all, which `symhow update` doesn't support incrementally",
        );
        return 2;
    }

    let repo_root = if !options.repo_root.is_empty() {
        options.repo_root.clone()
    } else {
        prov.repo_root.clone()
    };
    if repo_root.is_empty() && options.explicit_files.is_empty() {
        report_refused(
            options,
            "no repo root available (neither --repo nor a stored symhow_repo_root) and no --files given",
        );
        return 2;
    }

    let cdb_path = if !options.cdb_path.is_empty() {
        options.cdb_path.clone()
    } else {
        prov.cdb_path.clone()
    };
    if cdb_path.is_empty() {
        report_refused(
            options,
            "no compile_commands.json path available (neither -c nor a stored symhow_cdb_path)",
        );
        return 2;
    }

    // --- effective options: stored, overridden by CLI where given ----------
    let mut eff = EffectiveOptions {
        db_options: prov.db_options.clone(),
        sanitizer_options: prov.sanitizer_options.clone(),
        system_file_rows: prov.system_file_rows,
    };

    let mut diverges = false;
    if options.db_overrides_given {
        eff.db_options = options.db_overrides.clone();
        diverges = true;
    }
    if options.cuda_mode_given {
        eff.sanitizer_options.cuda_mode = options.cuda_mode_override;
        diverges = true;
    }
    if options.system_file_rows_given {
        eff.system_file_rows = options.system_file_rows_override;
        diverges = true;
    }
    if options.extra_arg_given {
        eff.sanitizer_options.extra_args = options.extra_arg_override.clone();
    }
    if options.drop_arg_given {
        eff.sanitizer_options.drop_arg_patterns = options.drop_arg_override.clone();
    }
    if options.std_given {
        eff.sanitizer_options.std_fallback = options.std_override.clone();
    }
    if options.c_std_given {
        eff.sanitizer_options.c_std_fallback = options.c_std_override.clone();
    }
    if options.exclude_submodules && !repo_root.is_empty() {
        for glob in submodule_exclude_globs(&repo_root) {
            if !eff.db_options.exclude_globs.contains(&glob) {
                eff.db_options.exclude_globs.push(glob);
                diverges = true;
            }
        }
    }
    if diverges {
        eprintln!(
            "symhow update: warning: CLI overrides change include/exclude globs, cuda mode, or system-file-rows -- the resulting database will diverge from a fresh `symhow index` with these options"
        );
    }

    // --- 2. compute changes --------------------------------------------------
    let mut changes: Vec<ChangedFile> = Vec::new();
    let mut dirty_paths: Vec<String> = prov.dirty_paths.clone();
    let mut new_baseline = prov.repo_commit.clone();

    if !options.explicit_files.is_empty() {
        for f in &options.explicit_files {
            changes.push(ChangedFile {
                kind: ChangeKind::Modified,
                old_path: String::new(),
                new_path: absolute_lexically_normal(f),
            });
        }
    } else {
        let baseline = if !options.from_rev.is_empty() {
            options.from_rev.clone()
        } else {
            prov.repo_commit.clone()
        };
        if baseline.is_empty() {
            report_refused(
                options,
                "no baseline commit available (neither --from nor a stored symhow_repo_commit)",
            );
            return 2;
        }
        let to_given = !options.to_rev.is_empty();
        let to_for_diff: Option<&str> = if to_given { Some(options.to_rev.as_str()) } else { None };

        let cs = match collect_changes(&repo_root, &baseline, to_for_diff) {
            Ok(cs) => cs,
            Err(err) => {
                report_error(options, &err);
                return 1;
            }
        };
        changes = cs.changes;

        let resolved = rev_parse(&repo_root, if to_given { options.to_rev.as_str() } else { "HEAD" });
        let Some(resolved) = resolved else {
            report_error(
                options,
                &format!(
                    "couldn't resolve '{}' to a commit",
                    if to_given { options.to_rev.as_str() } else { "HEAD" }
                ),
            );
            return 1;
        };
        new_baseline = resolved;

        if !to_given {
            dirty_paths = cs.dirty_paths;
            // Also fold in every stored-at-last-index dirty path that git
            // diff itself wouldn't re-report (e.g. a path that was already
            // dirty at the previous index/update and remains untouched
            // since): union by normalized path, added as Modified.
            let mut seen: HashSet<String> = HashSet::new();
            for c in &changes {
                seen.insert(normalize_path.resolve(&c.new_path));
            }
            for p in &prov.dirty_paths {
                if seen.insert(normalize_path.resolve(p)) {
                    changes.push(ChangedFile {
                        kind: ChangeKind::Modified,
                        old_path: String::new(),
                        new_path: p.clone(),
                    });
                }
            }
        }
    }

    summary.repo_root = repo_root.clone();
    summary.from_rev = if !options.explicit_files.is_empty() {
        String::new()
    } else if !options.from_rev.is_empty() {
        options.from_rev.clone()
    } else {
        prov.repo_commit.clone()
    };
    summary.to_rev = new_baseline.clone();

    // --- load CDB, existing symhow_tu rows ---------------------------------------
    let cdb: Vec<CompileEntry> = match load(&cdb_path, &eff.db_options) {
        Ok(cdb) => cdb,
        Err(e) => {
            report_error(options, &format!("failed to load compile database '{}': {}", cdb_path, e.0));
            return 1;
        }
    };

    let mut tu_rows: Vec<TuRow> = Vec::new();
    let mut db_file_paths: HashSet<String> = HashSet::new();
    let mut file_path_by_id: HashMap<i64, String> = HashMap::new();
    {
        let db = match Database::open(options.db_path.as_str(), 0) {
            Ok(db) => db,
            Err(e) => {
                report_error(options, &e.0);
                return 1;
            }
        };
        if table_exists(&db, "symhow_tu") {
            // Phase 4: symhow_tu gained a `status` column that replaces the
            // old (Phase 3) "no outgoing INCLUDE edge" retry heuristic with a
            // precise per-TU outcome. Later, symhow_tu gained an
            // `entry_count` column (the duplicate-CDB-entry fix --
            // args_hash became a COMBINED, per-path hash instead of a single
            // entry's) -- a symhow_tu table that predates either column
            // (PRAGMA table_info doesn't list it) is legacy: refuse, same as
            // any other provenance-shape mismatch -- there is no way to
            // retry-classify/flag-compare it correctly (in particular, a
            // pre-entry_count args_hash is a single entry's hash, not the
            // combined one this code now compares against).
            let mut have_status_column = false;
            let mut have_entry_count_column = false;
            if let Err(e) = db.run("PRAGMA table_info(symhow_tu)", [], |row| {
                let name: Option<String> = row.get(1)?;
                match name.as_deref() {
                    Some("status") => have_status_column = true,
                    Some("entry_count") => have_entry_count_column = true,
                    _ => {}
                }
                Ok(())
            }) {
                report_error(options, &e.0);
                return 1;
            }
            if !have_status_column {
                report_refused(
                    options,
                    "database's symhow_tu table predates per-TU status tracking (symhow_tu.status) -- re-index from scratch",
                );
                return 2;
            }
            if !have_entry_count_column {
                report_refused(
                    options,
                    "database's symhow_tu table predates duplicate-CDB-entry tracking (symhow_tu.entry_count) -- re-index from scratch",
                );
                return 2;
            }

            if let Err(e) = db.run("SELECT path, args_hash, file_node_id, status FROM symhow_tu", [], |row| {
                let path: Option<String> = row.get(0)?;
                let path = path.unwrap_or_default();
                let hash: Option<String> = row.get(1)?;
                let status: Option<String> = row.get(3)?;
                let row = TuRow {
                    path: path.clone(),
                    args_hash: hash.unwrap_or_default(),
                    status: status.unwrap_or_else(|| "ok".to_string()),
                };
                db_file_paths.insert(path);
                tu_rows.push(row);
                Ok(())
            }) {
                report_error(options, &e.0);
                return 1;
            }
        }
        if let Err(e) = db.run("SELECT id, path FROM file", [], |row| {
            let id: i64 = row.get(0)?;
            let path: Option<String> = row.get(1)?;
            file_path_by_id.insert(id, path.unwrap_or_default());
            Ok(())
        }) {
            report_error(options, &e.0);
            return 1;
        }
    }

    // Flag-change detection: a PATH's combined args hash differs from its
    // symhow_tu row -> Modified. A symhow_tu row whose path is absent from
    // the CDB -> Deleted. Compared per PATH, not per CDB entry: a real CDB
    // can list the same source file more than once (several targets/configs
    // with different flags), and symhow_tu keeps exactly one row per path --
    // so there is exactly one hash to compare against, computed once up
    // front from EVERY entry of that path together (not whichever entry
    // happens to be iterated), and both entries flag (or don't flag)
    // together rather than only whichever one is compared last winning.
    let mut tu_by_norm_path: HashMap<String, TuRow> = HashMap::new();
    for row in &tu_rows {
        tu_by_norm_path.insert(normalize_path.resolve(&row.path), row.clone());
    }
    // Suppress only content-derived changes, BEFORE adding flag changes or
    // retry seeds. --files and explicit revisions retain their force
    // semantics. A missing/unreadable file, absent/unknown fingerprint, or
    // new TU always takes the conservative path. Previous dirty paths still
    // participate: reverting one differs from its indexed hash and must be
    // re-indexed.
    if !changes.is_empty()
        && options.explicit_files.is_empty()
        && options.from_rev.is_empty()
        && options.to_rev.is_empty()
        && !diverges
    {
        let suppression: Result<()> = (|| {
            let db = Database::open(options.db_path.as_str(), 0)?;
            if table_exists(&db, "symhow_file_fingerprint") {
                let mut candidate_paths: HashSet<String> = HashSet::new();
                for change in &changes {
                    if change.kind == ChangeKind::Modified || change.kind == ChangeKind::Added {
                        candidate_paths.insert(normalize_path.resolve(&change.new_path));
                    }
                }
                let mut indexed_hashes: HashMap<String, String> = HashMap::new();
                // Fetch hashes only for changed candidates, not the whole corpus.
                for (id, path) in &file_path_by_id {
                    let norm = normalize_path.resolve(path);
                    if !candidate_paths.contains(&norm) {
                        continue;
                    }
                    db.run(
                        "SELECT fp.sha256 FROM symhow_file_fingerprint fp JOIN file f ON f.id=fp.file_node_id \
                         WHERE f.id=?1 AND f.indexed=1 AND f.complete=1",
                        [id],
                        |row| {
                            let hash: Option<String> = row.get(0)?;
                            if let Some(hash) = hash {
                                match indexed_hashes.entry(norm.clone()) {
                                    std::collections::hash_map::Entry::Vacant(v) => {
                                        v.insert(hash);
                                    }
                                    std::collections::hash_map::Entry::Occupied(mut o) => {
                                        if o.get() != &hash {
                                            o.get_mut().clear(); // ambiguous aliases: don't skip
                                        }
                                    }
                                }
                            }
                            Ok(())
                        },
                    )?;
                }
                let mut new_cdb_paths: HashSet<String> = HashSet::new();
                for entry in &cdb {
                    let norm = normalize_path.resolve(&entry.file);
                    if !tu_by_norm_path.contains_key(&norm) {
                        new_cdb_paths.insert(norm);
                    }
                }
                let mut disk_hashes: HashMap<String, Option<String>> = HashMap::new();
                let mut retained: Vec<ChangedFile> = Vec::with_capacity(changes.len());
                for change in changes.iter() {
                    if change.kind != ChangeKind::Modified && change.kind != ChangeKind::Added {
                        retained.push(change.clone()); // deletions/renames must invalidate path identity
                        continue;
                    }
                    let norm = normalize_path.resolve(&change.new_path);
                    if new_cdb_paths.contains(&norm) {
                        retained.push(change.clone()); // file was a header, but is now also a new TU
                        continue;
                    }
                    let Some(indexed) = indexed_hashes.get(&norm) else {
                        retained.push(change.clone());
                        continue;
                    };
                    if indexed.is_empty() {
                        retained.push(change.clone());
                        continue;
                    }
                    let disk = disk_hashes
                        .entry(norm)
                        .or_insert_with(|| file_content_hash(&change.new_path));
                    if !matches!(disk.as_deref(), Some(d) if d == indexed) {
                        retained.push(change.clone());
                    }
                }
                changes = retained;
            }
            Ok(())
        })();
        if let Err(e) = suppression {
            report_error(options, &format!("failed to compare indexed content: {}", e.0));
            return 1;
        }
    }

    let cdb_path_hashes = path_args_hashes(&cdb);
    let mut cdb_norm_paths: HashSet<String> = HashSet::new();
    let mut flagged_norm_paths: HashSet<String> = HashSet::new();
    let mut flag_changed_count: u32 = 0;
    for entry in &cdb {
        let norm = normalize_path.resolve(&entry.file);
        cdb_norm_paths.insert(norm.clone());
        let Some(row) = tu_by_norm_path.get(&norm) else {
            continue; // brand-new path, not a flag-change candidate
        };
        if !flagged_norm_paths.insert(norm) {
            continue; // already flagged (or checked) this path via an earlier entry
        }
        let combined_hash = cdb_path_hashes
            .get(&entry.file)
            .map(|info| info.combined_hash.clone())
            .unwrap_or_default();
        if combined_hash != row.args_hash {
            changes.push(ChangedFile {
                kind: ChangeKind::Modified,
                old_path: String::new(),
                new_path: entry.file.clone(),
            });
            flag_changed_count += 1;
        }
    }
    for row in &tu_rows {
        if !cdb_norm_paths.contains(&normalize_path.resolve(&row.path)) {
            changes.push(ChangedFile {
                kind: ChangeKind::Deleted,
                old_path: String::new(),
                new_path: row.path.clone(),
            });
        }
    }

    // Retry set (Phase 4): CDB entries whose symhow_tu.status from the
    // previous run is anything other than "ok" -- precise per-TU tracking,
    // replacing the old (Phase 3) "no outgoing INCLUDE edge"
    // over-approximation.
    let mut retry_tu_files: Vec<String> = Vec::new();
    for entry in &cdb {
        let norm = normalize_path.resolve(&entry.file);
        let Some(row) = tu_by_norm_path.get(&norm) else {
            continue; // brand-new TU, not a retry candidate
        };
        if row.status != "ok" {
            retry_tu_files.push(entry.file.clone());
        }
    }

    // Phase 8 resume: when resuming an interrupted `symhow index`, the retry
    // set must ALSO cover every CDB entry with NO symhow_tu row at all --
    // not just ones a `git diff` happened to flag as Added. This is the
    // "untracked suffix" the interrupted run never got to: since checkpoints
    // commit symhow_tu rows for a contiguous CDB prefix (the parallel
    // reorder buffer guarantees symhow_tu rows land in CDB order regardless
    // of -j), every CDB entry past the last checkpoint -- and any entry whose
    // file happens to have a `file` row already (e.g. reached as a header by
    // an earlier, completed TU) but was never itself indexed as a TU -- has
    // no symhow_tu row and lands here.
    if resuming_interrupted_index {
        for entry in &cdb {
            let norm = normalize_path.resolve(&entry.file);
            if !tu_by_norm_path.contains_key(&norm) {
                retry_tu_files.push(entry.file.clone());
            }
        }
    }

    // De-duplicate `changes`: a file that's both git-diff-detected AND
    // flag/CDB-derived (e.g. a deleted TU is simultaneously a real `git
    // diff` Deleted entry AND a "symhow_tu row has no CDB entry anymore"
    // synthetic Deleted entry above; likewise a TU whose content changed in
    // the same commit its compile flags changed would otherwise get a
    // git-diff Modified entry AND a flag-changed Modified entry) would
    // otherwise be counted/cleared twice.
    {
        let mut seen_change_keys: HashSet<String> = HashSet::new();
        let mut deduped: Vec<ChangedFile> = Vec::with_capacity(changes.len());
        for c in changes {
            let key = format!(
                "{}|{}|{}",
                change_kind_int(c.kind),
                normalize_path.resolve(&c.old_path),
                normalize_path.resolve(&c.new_path)
            );
            if seen_change_keys.insert(key) {
                deduped.push(c);
            }
        }
        changes = deduped;
    }

    // files.renamed (symhow.update-summary/1): count of Renamed entries git
    // diff detected.
    let mut renamed_count: u32 = 0;
    for c in &changes {
        if c.kind == ChangeKind::Renamed {
            renamed_count += 1;
        }
    }

    // --- 3. classify against the DB/CDB --------------------------------------
    let match_input = MatchInput {
        changes: changes.clone(),
        db_file_paths: db_file_paths.clone(),
        cdb: cdb.clone(),
    };
    let match_result = match_changed_with(&match_input, &mut normalize_path);

    summary.changed_count = match_result.changed_in_db.len() as u32;
    summary.deleted_count = match_result.deleted.len() as u32;
    summary.renamed_count = renamed_count;
    summary.flag_changed_count = flag_changed_count;
    summary.unmatched_count = match_result.unmatched.len() as u32;
    summary.unmatched_paths = match_result.unmatched.clone();
    if options.verbose > 0 {
        for u in &match_result.unmatched {
            eprintln!("symhow update: unmatched: {}", u);
        }
    }

    // compute_clear_set() matches its seed against `file.path` with an EXACT
    // string comparison -- so every candidate below is resolved back to its
    // precise file.path spelling via a normalized-path lookup before being
    // added. This also matters for a git-changed HEADER: match_changed()
    // only classifies against symhow_tu/CDB (TUs), so a changed header with
    // no CDB entry of its own always lands in `unmatched` -- but it's still
    // a real `file.path` row, and the whole point of the reverse-INCLUDE
    // closure (compute_clear_set step 1) is to discover which TUs include
    // it, so it MUST still be fed in as a seed here.
    let mut db_path_by_norm: HashMap<String, String> = HashMap::new();
    for (_id, path) in &file_path_by_id {
        db_path_by_norm.insert(normalize_path.resolve(path), path.clone());
    }

    let mut clear_seed_set: HashSet<String> = HashSet::new();
    let mut deleted_seed_set: HashSet<String> = HashSet::new();
    for p in &match_result.changed_in_db {
        clear_seed_set.insert(resolve_db_path(p, &db_path_by_norm, &mut normalize_path));
    }
    for r in &retry_tu_files {
        clear_seed_set.insert(resolve_db_path(r, &db_path_by_norm, &mut normalize_path));
    }
    for p in &match_result.deleted {
        deleted_seed_set.insert(resolve_db_path(p, &db_path_by_norm, &mut normalize_path));
    }
    for c in &changes {
        let Some(db_path) = db_path_by_norm.get(&normalize_path.resolve(&c.new_path)) else {
            continue; // not a known DB file at all (brand-new file) -- nothing to clear
        };
        if c.kind == ChangeKind::Deleted {
            deleted_seed_set.insert(db_path.clone());
        } else {
            clear_seed_set.insert(db_path.clone());
        }
    }
    let clear_seed: Vec<String> = clear_seed_set.into_iter().collect();
    let deleted_seed: Vec<String> = deleted_seed_set.into_iter().collect();

    // --- 4. nothing to do? ----------------------------------------------------
    // deleted_seed (not match.deleted!) is the right emptiness check here:
    // match.deleted only covers changed paths that already had a symhow_tu
    // row (an indexed TU), so a DELETED HEADER (never a TU itself, only ever
    // classified via the db_path_by_norm/changes loop above into
    // deleted_seed_set) would be invisible to match.deleted alone -- making
    // this branch wrongly declare "nothing to do" and skip clearing the
    // header (and its includers) entirely. deleted_seed is a strict superset
    // of match.deleted, so this is safe.
    if clear_seed.is_empty() && deleted_seed.is_empty() && match_result.new_tus.is_empty() {
        summary.nothing_to_do = true;
        // Never mutate the real database on a --dry-run -- this refresh is
        // "opportunistic bookkeeping for next time", not something a
        // dry-run is allowed to have side effects for.
        if (!repo_root.is_empty() || resuming_interrupted_index) && !options.dry_run {
            let refresh: Result<()> = (|| {
                let mut w = WritableDb::open(options.db_path.as_str(), false)?;
                w.begin()?;
                if !repo_root.is_empty() {
                    set_meta(&w, "symhow_repo_commit", &new_baseline)?;
                    let dirty_json = serde_json::to_string(&current_dirty_paths(&repo_root, &dirty_paths))
                        .map_err(|e| Error::new(e.to_string()))?;
                    set_meta(&w, "symhow_dirty_paths", &dirty_json)?;
                    set_meta(&w, "symhow_last_update_at", &utc_timestamp_now())?;
                }
                if resuming_interrupted_index {
                    // Every TU already has a symhow_tu row (the interrupted
                    // run must have reached the very last checkpoint before
                    // being interrupted) -- there's genuinely nothing left
                    // to index, so just clear the flag.
                    delete_meta(&w, "symhow_index_in_progress")?;
                }
                w.commit()
            })();
            // Best-effort only -- not a hard failure for "nothing to do".
            let _ = refresh;
        }
        summary.elapsed_ms = millis_since(run_start);
        if !options.json {
            eprintln!("symhow update: nothing to do");
        } else {
            println!("{}", build_summary_json(options, &summary));
        }
        if let Some(o) = out {
            *o = summary;
        }
        return 5;
    }

    // --- 5. working copy ------------------------------------------------------
    let mut work = options.db_path.clone();
    let mut work_proj = String::new();
    let mut made_temp_copy = false;
    if !options.in_place {
        let dbp = Path::new(&options.db_path);
        let stem = dbp.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = dbp
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        work = dbp
            .parent()
            .unwrap_or(Path::new(""))
            .join(format!("{}.incr-{}{}", stem, std::process::id(), ext))
            .to_string_lossy()
            .into_owned();
        if let Err(e) = std::fs::copy(&options.db_path, &work) {
            report_error(options, &format!("failed to create working copy '{}': {}", work, e));
            return 1;
        }
        made_temp_copy = true;
        work_proj = proj_sidecar_for(&work);
    } else if !options.dry_run {
        // NEVER write symhow_update_in_progress on a --dry-run -- the whole
        // point of --in-place --dry-run is to be side-effect-free. Setting
        // this flag here and only clearing it after a real re-index (step
        // 9, far below, which a dry-run's early return skips entirely)
        // used to brick the database: every later `symhow update` would see
        // the stale flag and refuse forever, with no real update ever
        // having modified anything.
        let set_flag: Result<()> = (|| {
            let mut w = WritableDb::open(options.db_path.as_str(), false)?;
            w.begin()?;
            set_meta(&w, "symhow_update_in_progress", "1")?;
            w.commit()
        })();
        if let Err(e) = set_flag {
            report_error(options, &format!("failed to set in-progress flag: {}", e.0));
            return 1;
        }
    }

    let plan: ClearPlan = {
        let computed: Result<ClearPlan> = (|| {
            let mut w = WritableDb::open(work.as_str(), true)?;
            let mut tu_paths_for_closure: Vec<String> = Vec::with_capacity(cdb.len());
            for e in &cdb {
                // Resolve through the SAME normalizePath()-based mapping used
                // for the clear/deleted seeds above (db_path_by_norm/
                // resolve_db_path) rather than pushing the CDB's raw spelling
                // -- the header-pruning step excludes these paths via an
                // EXACT string match against `file.path`, so a `./`-prefixed
                // or symlinked CDB entry that textually differs from its
                // file.path row would otherwise fail to match, and a header
                // that's ALSO a TU could get wrongly pruned (violating the
                // "a TU is never pruned as a header" invariant).
                tu_paths_for_closure.push(resolve_db_path(&e.file, &db_path_by_norm, &mut normalize_path));
            }
            compute_clear_set(&mut w, &clear_seed, &deleted_seed, &tu_paths_for_closure)
        })();
        match computed {
            Ok(plan) => plan,
            Err(e) => {
                report_error(options, &format!("failed to compute clear set: {}", e.0));
                cleanup_temp(made_temp_copy, &work, &work_proj);
                return 1;
            }
        }
    };

    summary.plan = plan.clone();

    // --- 5.5: work out what would be (or will be) re-indexed -------------------
    // Computed unconditionally (used by both --dry-run's JSON summary below
    // and the real re-index step further down) so a dry run can report
    // accurate tus.* counts instead of the caller having to guess.
    let mut reindex_norm: HashSet<String> = HashSet::new();
    for p in &plan.cleared_paths {
        reindex_norm.insert(normalize_path.resolve(p));
    }
    for p in &match_result.new_tus {
        reindex_norm.insert(normalize_path.resolve(p));
    }
    for p in &retry_tu_files {
        reindex_norm.insert(normalize_path.resolve(p));
    }

    let preset_entries = select_entries_with(&cdb, &reindex_norm, &mut normalize_path);
    let mut filtered_entries: Vec<CompileEntry> = Vec::with_capacity(preset_entries.len());
    let mut skipped_missing: u32 = 0;
    for entry in preset_entries {
        if !Path::new(&entry.file).exists() {
            skipped_missing += 1;
            if options.verbose > 0 {
                eprintln!("symhow update: warning: skipping missing file '{}'", entry.file);
            }
            continue;
        }
        filtered_entries.push(entry);
    }

    summary.tus_to_index = filtered_entries.len() as u32;
    summary.tus_new = match_result.new_tus.len() as u32;
    summary.tus_retried = retry_tu_files.len() as u32;
    summary.skipped_missing = skipped_missing;

    if options.dry_run {
        eprintln!(
            "symhow update: dry-run: would clear {} files ({} seed, {} via includes, {} pruned headers); would re-index {} TUs ({} new, {} retried, {} skipped-missing)",
            plan.cleared_file_ids.len(),
            plan.seed_files,
            plan.include_closure_added,
            plan.headers_pruned,
            summary.tus_to_index,
            summary.tus_new,
            summary.tus_retried,
            summary.skipped_missing
        );
        if options.verbose > 0 {
            for p in &plan.cleared_paths {
                eprintln!("  clear: {}", p);
            }
            for t in &match_result.new_tus {
                eprintln!("  new tu: {}", t);
            }
        }
        cleanup_temp(made_temp_copy, &work, &work_proj);
        summary.elapsed_ms = millis_since(run_start);
        if options.json {
            println!("{}", build_summary_json(options, &summary));
        }
        if let Some(o) = out {
            *o = summary;
        }
        return 0;
    }

    // --- 6. invalidate ----------------------------------------------------
    let mut gc: GcStats = {
        let invalidated: Result<GcStats> = (|| {
            let mut w = WritableDb::open(work.as_str(), true)?;
            invalidate(&mut w, &plan)
        })();
        match invalidated {
            Ok(stats) => stats,
            Err(e) => {
                report_error(options, &format!("invalidation failed: {}", e.0));
                cleanup_temp(made_temp_copy, &work, &work_proj);
                return 1;
            }
        }
    };

    // --- 7. re-index ----------------------------------------------------------
    let mut idx_summary = IndexSummary::default();
    let index_start = Instant::now();
    if !filtered_entries.is_empty() {
        let run_opts = IndexRunOptions {
            compile_commands_path: cdb_path.clone(),
            output_path: work.clone(),
            append_to_existing: true,
            preset_entries: Some(filtered_entries.clone()),
            quiet: options.verbose <= 0,
            json_summary: false,
            no_vacuum: options.no_vacuum,
            verbose: options.verbose > 0,
            parallel: options.parallel,
            symhow_version: options.symhow_version.clone(),
            db_options: eff.db_options.clone(),
            sanitizer_options: eff.sanitizer_options.clone(),
            system_file_rows: eff.system_file_rows,
            ..IndexRunOptions::default()
        };

        let mut updated = prov.clone();
        updated.symhow_version = options.symhow_version.clone();
        updated.walker_version = K_WALKER_VERSION;
        updated.cdb_path = absolute_lexically_normal(&cdb_path);
        updated.db_options = eff.db_options.clone();
        updated.sanitizer_options = eff.sanitizer_options.clone();
        updated.system_file_rows = eff.system_file_rows;
        updated.repo_root = repo_root.clone();
        updated.repo_commit = new_baseline.clone();
        updated.dirty_paths = current_dirty_paths(&repo_root, &dirty_paths);
        updated.last_update_at_utc = utc_timestamp_now();

        let rc = run_index(&run_opts, Some(&mut idx_summary));
        if rc == 1 {
            report_error(options, "re-indexing failed");
            if !options.in_place {
                cleanup_temp(made_temp_copy, &work, &work_proj);
            } else {
                eprintln!(
                    "symhow update: the database was modified in place and left with symhow_update_in_progress set -- re-index from scratch to recover"
                );
            }
            return 1;
        }
        summary.index_ran = true;
        summary.idx_tus_total = idx_summary.tus_total;
        summary.idx_tus_attempted = idx_summary.tus_attempted;
        summary.idx_tus_with_content = idx_summary.tus_with_content;
        summary.idx_tus_failed = idx_summary.tus_failed;
        summary.idx_symbols = idx_summary.symbols;
        summary.idx_references = idx_summary.references;
        summary.idx_files = idx_summary.files;
        summary.idx_errors = idx_summary.errors;
        summary.idx_name_collisions = idx_summary.name_collisions;
    } else {
        // Nothing to re-index (everything cleared was a pure deletion) --
        // still refresh provenance so symhow_repo_commit/dirty paths/
        // last-update-at stay current.
        let refreshed: Result<()> = (|| {
            let mut w = WritableDb::open(work.as_str(), false)?;
            w.begin()?;
            let mut updated = prov.clone();
            updated.symhow_version = options.symhow_version.clone();
            updated.repo_root = repo_root.clone();
            updated.repo_commit = new_baseline.clone();
            updated.dirty_paths = current_dirty_paths(&repo_root, &dirty_paths);
            updated.last_update_at_utc = utc_timestamp_now();
            write_provenance(&w, &updated)?;
            w.commit()
        })();
        if let Err(e) = refreshed {
            report_error(options, &format!("failed to refresh provenance: {}", e.0));
            cleanup_temp(made_temp_copy, &work, &work_proj);
            return 1;
        }
    }
    summary.index_elapsed_ms = millis_since(index_start);

    // --- 8. post-index sweep ---------------------------------------------------
    {
        let swept: Result<GcStats> = (|| {
            let mut w2 = WritableDb::open(work.as_str(), true)?;
            let sweep = sweep_orphans(&mut w2)?;
            w2.exec("ANALYZE;")?;
            Ok(sweep)
        })();
        match swept {
            Ok(sweep) => {
                gc.edges += sweep.edges;
                gc.locations += sweep.locations;
                gc.nodes += sweep.nodes;
                gc.local_symbols += sweep.local_symbols;
                gc.errors += sweep.errors;
                gc.non_indexed_files += sweep.non_indexed_files;
                gc.orphan_elements += sweep.orphan_elements;
            }
            Err(e) => {
                eprintln!("symhow update: warning: post-index sweep failed: {}", e.0);
            }
        }
    }
    summary.gc = gc.clone();

    // --- 8.5. notes sync (Phase 3 of docs/NOTES_DESIGN.md) ---------------------
    // Best-effort: resolve a notes dir exactly like the CLI's
    // resolve-notes-default logic (meta.symhow_notes_dir, else <repo
    // root>/notes if it exists), and if one exists on disk, re-run
    // sync_notes() against the just-re-indexed working copy so
    // `symhow_note.stale` reflects the new code before this update is
    // handed back to the caller. Runs BEFORE the
    // symhow_index_in_progress clear/rename below so it's part of the same
    // working-copy state that becomes the final database. Never fails the
    // update -- any error here is reported and swallowed.
    {
        let notes: Result<()> = (|| {
            let notes_dir: String;
            {
                let rdb = Database::open(work.as_str(), 0)?;
                let stored = get_meta(rdb.conn(), "symhow_notes_dir");
                if stored.as_deref().map_or(false, |s| !s.is_empty()) {
                    notes_dir = stored.unwrap_or_default();
                } else if !repo_root.is_empty() {
                    let candidate = Path::new(&repo_root).join("notes");
                    if candidate.is_dir() {
                        notes_dir = candidate.to_string_lossy().into_owned();
                    } else {
                        notes_dir = String::new();
                    }
                } else {
                    notes_dir = String::new();
                }
            }

            if !notes_dir.is_empty() && Path::new(&notes_dir).is_dir() {
                // Pre-sync stale set, straight from whatever the previous
                // `symhow notes sync` left in `symhow_note` -- re-indexing
                // itself never touches this cache, so it's still exactly
                // last sync's result.
                let mut pre_stale: HashSet<String> = HashSet::new();
                {
                    let rdb = Database::open(work.as_str(), 0)?;
                    // symhow_note doesn't exist yet (never synced) -- pre_stale
                    // stays empty.
                    let _ = rdb.run("SELECT serialized_name FROM symhow_note WHERE stale = 1", [], |row| {
                        let t: Option<String> = row.get(0)?;
                        if let Some(t) = t {
                            pre_stale.insert(t);
                        }
                        Ok(())
                    });
                }

                let sync_result = sync_notes(&work, &notes_dir, false)?;

                let mut post_stale: HashSet<String> = HashSet::new();
                {
                    let rdb = Database::open(work.as_str(), 0)?;
                    rdb.run("SELECT serialized_name FROM symhow_note WHERE stale = 1", [], |row| {
                        let t: Option<String> = row.get(0)?;
                        if let Some(t) = t {
                            post_stale.insert(t);
                        }
                        Ok(())
                    })?;
                }
                let mut newly_stale: u32 = 0;
                for sn in &post_stale {
                    if !pre_stale.contains(sn) {
                        newly_stale += 1;
                    }
                }

                summary.notes_ran = true;
                summary.notes_noted = sync_result.counts.matched as u32;
                summary.notes_stale = sync_result.counts.stale as u32;
                summary.notes_newly_stale = newly_stale;
                summary.notes_problems = sync_result.problems.len() as u32;

                if !options.json {
                    eprintln!(
                        "notes: {} noted, {} stale ({} newly stale)",
                        summary.notes_noted, summary.notes_stale, summary.notes_newly_stale
                    );
                }
            }
            Ok(())
        })();
        if let Err(e) = notes {
            eprintln!("symhow update: warning: post-update notes sync failed: {}", e.0);
        }
    }

    // Phase 8: clear symhow_index_in_progress on `work` -- before the rename/
    // in-place-commit step below, so it's gone from whichever path
    // ultimately becomes `options.db_path`. Idempotent (no-op if absent), so
    // safe to run unconditionally rather than gating on
    // resuming_interrupted_index (a belt-and-suspenders cleanup: e.g. a copy
    // made via `cp` of an in-progress db that `symhow index` itself never
    // got a chance to clear).
    {
        let cleared: Result<()> = (|| {
            let mut wflag = WritableDb::open(work.as_str(), false)?;
            wflag.begin()?;
            delete_meta(&wflag, "symhow_index_in_progress")?;
            wflag.commit()
        })();
        if let Err(e) = cleared {
            eprintln!("symhow update: warning: failed to clear symhow_index_in_progress: {}", e.0);
        }
    }

    // --- 9. commit --------------------------------------------------------------
    if !options.in_place {
        if let Err(e) = std::fs::rename(&work, &options.db_path) {
            report_error(
                options,
                &format!("failed to rename '{}' to '{}': {}", work, options.db_path, e),
            );
            cleanup_temp(made_temp_copy, &work, &work_proj);
            return 1;
        }
        let _ = std::fs::remove_file(format!("{}-journal", work));
        let _ = std::fs::remove_file(&work_proj);
    } else {
        let cleared: Result<()> = (|| {
            let mut w = WritableDb::open(options.db_path.as_str(), false)?;
            w.begin()?;
            delete_meta(&w, "symhow_update_in_progress")?;
            w.commit()
        })();
        if let Err(e) = cleared {
            eprintln!(
                "symhow update: warning: failed to clear symhow_update_in_progress after a successful update: {}",
                e.0
            );
        }
    }

    summary.elapsed_ms = millis_since(run_start);

    if !options.json {
        eprintln!(
            "symhow update: cleared {} files ({} via includes, {} pruned headers); re-indexed {} TUs ({} new, {} retried, {} skipped-missing); gc: {} nodes, {} edges, {} locations, {} symbols reset; {} ms",
            plan.cleared_file_ids.len(),
            plan.include_closure_added,
            plan.headers_pruned,
            summary.tus_to_index,
            summary.tus_new,
            summary.tus_retried,
            summary.skipped_missing,
            gc.nodes,
            gc.edges,
            gc.locations,
            gc.symbols_reset,
            summary.elapsed_ms
        );
    } else {
        println!("{}", build_summary_json(options, &summary));
    }

    if let Some(o) = out {
        *o = summary;
    }
    0
}
