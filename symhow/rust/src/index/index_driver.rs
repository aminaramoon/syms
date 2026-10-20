//! Orchestrates a full (non-dry-run) `symhow index` run: load the compile
//! database, then for each entry sanitize -> parse -> walk -> hand the
//! resulting TuFactBatch to a SrctrldbBackend, with a parallel worker pool
//! feeding a strictly-in-CDB-order single writer. This is where the real
//! CUDA rung retry ladder lives. Mirrors `symhow::index::IndexDriver`
//! (src/index/IndexDriver.cpp).

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use clang_sys::*;

use crate::common::content_hash::content_hash;
use crate::common::paths::normalize;
use crate::common::Result;
use crate::db::meta::{get_meta, set_meta};
use crate::db::{Database, WritableDb};
use crate::index::arg_sanitizer::{sanitize, SanitizerOptions};
use crate::index::ast_walker::AstWalker;
use crate::index::clangutil::{cx_string_to_string, ensure_libclang_loaded, file_name};
use crate::index::compile_db::{
    load, path_args_hashes, CompileDbOptions, CompileEntry, PathArgsInfo,
};
use crate::index::cuda_support::CudaMode;
use crate::index::file_registry::{FileKey, FileRegistry, FileRegistryOptions, SystemFileRows};
use crate::index::progress::{format_progress_line, format_running_summary, relative_to_root};
use crate::index::provenance::{write_provenance, Provenance};
use crate::index::srctrldb_backend::{PendingTuRow, SrctrldbBackend};
use crate::index::tu_parser::{parse_tu, ParsedTu};
use crate::model::{BatchSink, FactSink, FileFact, IndexBackend, IndexSummary, SourceRange, TuFactBatch, K_NO_LOCAL};

// Set by the SIGINT/SIGTERM handler below -- a signal-safe atomic flag, no
// I/O or allocation in the handler itself. Checked once per consumed TU by
// the (single) consumer thread in run_index(); real signal delivery is
// therefore best-effort ("interrupted after K/N TUs", not an exact K) --
// --stop-after (checked the same way, but driven by a plain counter instead
// of a signal) is what gives tests an EXACT, deterministic interruption point.
static G_INTERRUPT_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_interrupt_signal(_sig: libc::c_int) {
    G_INTERRUPT_REQUESTED.store(true, Ordering::Relaxed);
}

/// Installed once per run_index() call (idempotent: re-installing the same
/// trivial handler is harmless, mirroring the C++ std::signal behavior).
fn install_interrupt_handlers() {
    let handler = handle_interrupt_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

fn ends_with_cu(path: &str) -> bool {
    path.ends_with(".cu")
}

fn millis_since(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// Options for a full `symhow index` run. Mirrors
/// `symhow::index::IndexRunOptions`.
#[derive(Debug, Clone)]
pub struct IndexRunOptions {
    pub compile_commands_path: String,
    pub output_path: String,
    pub force: bool,
    /// -1 = no limit.
    pub limit: i64,

    pub quiet: bool,
    /// Set when the user passed -q/--quiet twice (symhow CLI); suppresses
    /// even the periodic (every-60s) summary line that a single -q keeps.
    pub super_quiet: bool,
    pub json_summary: bool,
    pub no_vacuum: bool,
    pub verbose: bool,

    /// --parallel/-j N: number of worker threads used to extract TUs
    /// (sanitize -> parse -> walk). 1 (default) = fully sequential.
    /// 0 = std::thread::available_parallelism(). N > 1 runs a fixed pool of
    /// N worker threads, but the single SrctrldbBackend writer is still fed
    /// batches strictly in CDB order (the reorder buffer below), so the
    /// RESULT is deterministic and identical regardless of N -- only
    /// wall-clock time changes.
    pub parallel: i32,

    /// Only used when json_summary is set: the "symhow_version" field of the
    /// symhow.index-summary/1 envelope (SYMHOW_VERSION_STRING) -- passed in
    /// from the CLI target rather than baked in here.
    pub symhow_version: String,

    /// Remap + include/exclude globs.
    pub db_options: CompileDbOptions,
    /// cuda_mode here is the user's requested default (Auto unless
    /// overridden).
    pub sanitizer_options: SanitizerOptions,
    pub system_file_rows: SystemFileRows,

    /// When true, skip the exists/--force delete guard entirely: the output
    /// path is opened and appended to (or created fresh) rather than ever
    /// being deleted. `force` is ignored in this mode.
    pub append_to_existing: bool,

    /// When set, skip load() entirely and index exactly these entries
    /// (already remapped/filtered by the caller) instead of re-parsing
    /// compile_commands_path from scratch.
    pub preset_entries: Option<Vec<CompileEntry>>,

    /// When set, written to `meta` right after begin() succeeds -- i.e.
    /// BEFORE any TU is consumed -- together with
    /// `symhow_index_in_progress="1"`.
    pub provenance: Option<Provenance>,

    /// Crash-safe/resumable indexing (checkpoints): after every
    /// `commit_every` consumed TUs -- and once more at the end -- the
    /// consumer thread checkpoints. 0 disables checkpointing entirely (one
    /// single transaction for the whole run). Default 25.
    pub commit_every: i32,

    /// Hidden testing hook: if >= 0, index exactly this many TUs and then
    /// behave exactly as if SIGINT/SIGTERM had just been delivered (final
    /// checkpoint, print the "interrupted" message, exit 130).
    pub stop_after: i64,
}

impl Default for IndexRunOptions {
    fn default() -> Self {
        IndexRunOptions {
            compile_commands_path: String::new(),
            output_path: "./symhow.srctrldb".to_string(),
            force: false,
            limit: -1,
            quiet: false,
            super_quiet: false,
            json_summary: false,
            no_vacuum: false,
            verbose: false,
            parallel: 1,
            symhow_version: String::new(),
            db_options: CompileDbOptions::default(),
            sanitizer_options: SanitizerOptions::default(),
            system_file_rows: SystemFileRows::Direct,
            append_to_existing: false,
            preset_entries: None,
            provenance: None,
            commit_every: 25,
            stop_after: -1,
        }
    }
}

/// Writes `prov` into `output_path`'s `meta` table (plus
/// `symhow_index_in_progress="1"`), in its own tiny transaction on a
/// SEPARATE short-lived WritableDb connection -- called right after
/// SrctrldbBackend::begin() (which opens the file and creates the schema,
/// but doesn't start the writer's transaction), BEFORE any TU is consumed.
/// No-op if `prov` isn't set.
fn write_provenance_early(output_path: &str, prov: Option<&Provenance>) -> Result<()> {
    let Some(prov) = prov else {
        return Ok(());
    };
    let mut db = WritableDb::open(output_path, /* foreign_keys= */ false)?;
    db.begin()?;
    write_provenance(&db, prov)?;
    set_meta(&db, "symhow_index_in_progress", "1")?;
    db.commit()
}

/// Records a single-file, single-error "couldn't even try" batch (used for
/// failures from sanitize, a --cuda-mode skip, or a parse producing no
/// usable TU at all).
fn minimal_failure_batch(path: &str, message: &str, fatal: bool) -> TuFactBatch {
    let mut sink = BatchSink::default();
    let ff = FileFact {
        path: path.to_string(),
        language: "cpp".to_string(),
        indexed: true,
        ..Default::default()
    };
    let fid = sink.file(ff);
    let r = SourceRange::new(fid, 1, 1, 1, 1);
    sink.error(message.to_string(), fatal, Some(&r));
    sink.take()
}

// --- libclang file resolution for diagnostics (mirrors AstWalker's private
// helpers; duplicated here because AstWalker keeps them private) -----------

fn diag_file_key(file: CXFile) -> FileKey {
    let mut id = CXFileUniqueID { data: [0u64; 3] };
    if unsafe { clang_getFileUniqueID(file, &mut id) } != 0 {
        return FileKey { a: file as usize as u64, b: 0, c: 0 };
    }
    FileKey { a: id.data[0], b: id.data[1], c: id.data[2] }
}

fn diag_file_is_system(tu: CXTranslationUnit, file: CXFile) -> bool {
    unsafe { clang_Location_isInSystemHeader(clang_getLocationForOffset(tu, file, 0)) != 0 }
}

fn diag_file_content_hash(tu: CXTranslationUnit, file: CXFile) -> String {
    let mut size: usize = 0;
    let contents = unsafe { clang_getFileContents(tu, file, &mut size) };
    if contents.is_null() {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(contents as *const u8, size) };
    content_hash(bytes)
}

fn append_diagnostics(
    parsed: &ParsedTu,
    files: &mut FileRegistry,
    sink: &mut BatchSink,
    any_fatal: &mut bool,
) {
    for d in &parsed.diagnostics {
        let mut file_id = K_NO_LOCAL;
        if d.has_location && !d.file.is_empty() {
            let c_file = std::ffi::CString::new(d.file.as_str()).unwrap_or_default();
            let file = unsafe { clang_getFile(parsed.tu.get(), c_file.as_ptr()) };
            if !file.is_null() {
                let key = diag_file_key(file);
                let path = file_name(file);
                let system = diag_file_is_system(parsed.tu.get(), file);
                let content_hash = if system { None } else { Some(diag_file_content_hash(parsed.tu.get(), file)) };
                let info = files.info_for(key, &path, system, content_hash).clone();
                let ff = FileFact {
                    path: info.path.clone(),
                    language: "cpp".to_string(),
                    indexed: info.indexed,
                    system: info.system,
                    content_hash: info.content_hash.clone(),
                };
                file_id = sink.file(ff);
            }
        }
        let range = SourceRange::new(file_id, d.line, d.col, d.line, d.col);
        sink.error(
            d.message.clone(),
            d.fatal,
            if file_id != K_NO_LOCAL { Some(&range) } else { None },
        );
        if d.fatal {
            *any_fatal = true;
        }
    }
}

/// One independent extraction attempt: sanitize (forcing `rung`) -> parse ->
/// walk. Never merges with any other attempt's facts -- the CUDA retry loop
/// in process_entry() picks exactly one winning attempt's batch (or
/// synthesizes its own all-rungs-failed batch).
struct RungResult {
    batch: TuFactBatch,
    content_ok: bool,
    parse_ms: u64,
    walk_ms: u64,
    diag_errors: u32,
    diag_fatal: u32,
}

impl Default for RungResult {
    fn default() -> Self {
        RungResult {
            batch: TuFactBatch::default(),
            content_ok: false,
            parse_ms: 0,
            walk_ms: 0,
            diag_errors: 0,
            diag_fatal: 0,
        }
    }
}

fn try_rung(
    entry: &CompileEntry,
    sanitizer_options: &SanitizerOptions,
    rung: CudaMode,
    file_reg_options: &FileRegistryOptions,
) -> RungResult {
    let mut sanitizer_options = sanitizer_options.clone();
    sanitizer_options.cuda_mode = rung;

    let mut result = RungResult::default();

    let san_result = sanitize(entry, &sanitizer_options);

    if san_result.skipped {
        result.batch = minimal_failure_batch(
            &entry.file,
            "symhow: translation unit skipped (--cuda-mode skip)",
            false,
        );
        result.content_ok = false;
        return result;
    }

    let mut sink = BatchSink::default();
    let mut files = FileRegistry::new(file_reg_options.clone());

    let parse_start = Instant::now();
    let parsed = parse_tu(&san_result.args);
    result.parse_ms = millis_since(parse_start);
    result.diag_errors = parsed.diag_errors;
    result.diag_fatal = parsed.diag_fatal;

    if !parsed.ok {
        result.batch = minimal_failure_batch(
            &entry.file,
            "symhow: clang_parseTranslationUnit2 failed to produce a usable translation unit",
            true,
        );
        result.content_ok = false;
        return result;
    }

    let mut any_fatal = false;
    let walk_start = Instant::now();
    {
        let mut walker = AstWalker::new(parsed.tu.get(), &mut files, &mut sink);
        walker.run();
    }
    append_diagnostics(&parsed, &mut files, &mut sink, &mut any_fatal);
    result.walk_ms = millis_since(walk_start);

    if !sink.batch().has_content() {
        let main_path = cx_string_to_string(unsafe { clang_getTranslationUnitSpelling(parsed.tu.get()) });
        let ff = FileFact {
            path: if main_path.is_empty() { entry.file.clone() } else { main_path },
            language: "cpp".to_string(),
            indexed: true,
            ..Default::default()
        };
        let fid = sink.file(ff);
        if sink.batch().errors.is_empty() {
            let message = format!(
                "symhow: translation unit produced no content ({} error diagnostics)",
                parsed.diag_errors + parsed.diag_fatal
            );
            let r = SourceRange::new(fid, 1, 1, 1, 1);
            sink.error(message, false, Some(&r));
        }
    }

    result.batch = sink.take();
    result.content_ok = result.batch.has_content() && !any_fatal;
    result
}

/// Outcome of fully processing one CompileEntry: sanitize -> (CUDA retry
/// ladder if applicable) -> parse -> walk. Self-contained (a fresh CXIndex,
/// FileRegistry and BatchSink per call) and side-effect-free w.r.t. any
/// shared state, so it is safe to run on a worker thread (see run_index's
/// parallel path) -- the only shared-state step left,
/// SrctrldbBackend::consume(), is deliberately NOT part of this.
struct TuOutcome {
    batch: TuFactBatch,
    rung_index: u32,
    has_content: bool,
    /// Persisted symhow_tu.status: "ok" / "partial" / "failed".
    status: String,
    extract_seconds: f64,
}

impl Default for TuOutcome {
    fn default() -> Self {
        TuOutcome {
            batch: TuFactBatch::default(),
            rung_index: 0,
            has_content: false,
            status: String::new(),
            extract_seconds: 0.0,
        }
    }
}

fn process_entry_inner(
    entry: &CompileEntry,
    options: &IndexRunOptions,
    file_reg_options: &FileRegistryOptions,
) -> TuOutcome {
    let mut out = TuOutcome::default();

    let is_cuda = ends_with_cu(&entry.file);
    let requested_mode = options.sanitizer_options.cuda_mode;

    const K_RUNGS: [CudaMode; 3] = [CudaMode::Full, CudaMode::NoWrapper, CudaMode::HostOnly];

    let mut rung_index: u32 = 0;
    let mut best: RungResult;

    if is_cuda && requested_mode == CudaMode::Auto {
        let mut ok = false;
        best = RungResult::default();
        for (i, rung) in K_RUNGS.iter().enumerate() {
            best = try_rung(entry, &options.sanitizer_options, *rung, file_reg_options);
            if best.content_ok {
                rung_index = (i + 1) as u32;
                ok = true;
                break;
            }
        }
        if !ok {
            best.batch = minimal_failure_batch(
                &entry.file,
                "symhow: all CUDA rungs (full, nowrapper, host-only) failed to produce content for this translation unit",
                true,
            );
            best.content_ok = false;
            rung_index = 4;
        }
    } else {
        // Non-CUDA file, or an explicit (non-auto) --cuda-mode: a single,
        // direct attempt -- no retry.
        best = try_rung(entry, &options.sanitizer_options, requested_mode, file_reg_options);
        rung_index = 0;
    }

    best.batch.stats.tu_file = normalize(&entry.file);
    best.batch.stats.parsed_rung_index = rung_index;
    best.batch.stats.parsed_ok = best.content_ok;
    best.batch.stats.parse_ms = best.parse_ms;
    best.batch.stats.walk_ms = best.walk_ms;
    best.batch.stats.diag_errors = best.diag_errors;
    best.batch.stats.diag_fatal = best.diag_fatal;

    out.has_content = best.batch.has_content();
    out.rung_index = rung_index;

    // Per-TU status persisted into symhow_tu.status -- "failed" (no
    // content), "partial" (content but a fatal diagnostic, or a CUDA run
    // that only succeeded on a degraded fallback rung), "ok" otherwise.
    if !out.has_content {
        out.status = "failed".to_string();
    } else if best.batch.stats.diag_fatal > 0 || rung_index >= 2 {
        out.status = "partial".to_string();
    } else {
        out.status = "ok".to_string();
    }

    out.batch = best.batch;
    out
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return Some((*s).to_string());
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return Some(s.clone());
    }
    None
}

/// The full per-entry extraction, wrapped in a last-resort panic net (the
/// C++ equivalent is try/catch around the whole body): a worker thread must
/// never let a panic escape and take the process down.
fn process_entry(
    entry: &CompileEntry,
    options: &IndexRunOptions,
    file_reg_options: &FileRegistryOptions,
) -> TuOutcome {
    let entry_start = Instant::now();
    let attempt = std::panic::catch_unwind(AssertUnwindSafe(|| {
        process_entry_inner(entry, options, file_reg_options)
    }));

    let mut out = match attempt {
        Ok(out) => out,
        Err(payload) => {
            // Defensive: every known failure step above already turns its
            // own failure into a minimal_failureBatch internally, but a
            // worker thread must never let a panic escape.
            let mut out = TuOutcome::default();
            let message = match panic_message(payload.as_ref()) {
                Some(m) => format!("symhow: unexpected exception during extraction: {}", m),
                None => "symhow: unknown exception during extraction".to_string(),
            };
            out.batch = minimal_failure_batch(&entry.file, &message, true);
            out.batch.stats.tu_file = normalize(&entry.file);
            out.has_content = false;
            out.status = "failed".to_string();
            out.rung_index = 0;
            out
        }
    };

    out.extract_seconds = entry_start.elapsed().as_secs_f64();
    out
}

// --- per-path status merge ----------------------------------------------------

fn status_rank(status: &str) -> i32 {
    if status == "failed" {
        2
    } else if status == "partial" {
        1
    } else {
        0
    }
}

/// Worse of `a` and `b` under status_rank() -- ties keep `a` (arbitrary but
/// deterministic).
fn worse_status<'a>(a: &'a str, b: &'a str) -> &'a str {
    if status_rank(b) > status_rank(a) {
        b
    } else {
        a
    }
}

/// The display status shown on a per-TU progress line ("ok" / "ok (cuda rung
/// N)" / "no content" / "failed") -- display-only; TuOutcome::status
/// (ok/partial/failed) is what lands in symhow_tu.status.
fn display_status_for(outcome: &TuOutcome) -> String {
    let mut label = if outcome.has_content {
        "ok".to_string()
    } else {
        let fatal_present = outcome.batch.errors.iter().any(|e| e.fatal);
        if fatal_present {
            "failed".to_string()
        } else {
            "no content".to_string()
        }
    };
    if outcome.rung_index > 0 {
        label += &format!(" (cuda rung {})", outcome.rung_index);
    }
    label
}

// --- progress reporting --------------------------------------------------------

/// Formats and prints (to stderr, flushed) the per-TU progress line and
/// periodic running-summary lines. Only ever touched from the single
/// consumer thread, strictly in CDB order -- never from a worker thread.
struct ProgressReporter {
    total: u32,
    quiet_level: i32,
    repo_root: String,
    start: Instant,
    last_summary: Instant,
    done: u32,
    last_summary_done: u32,
}

impl ProgressReporter {
    fn new(total: u32, quiet_level: i32, repo_root: String) -> ProgressReporter {
        let now = Instant::now();
        ProgressReporter {
            total,
            quiet_level,
            repo_root,
            start: now,
            last_summary: now,
            done: 0,
            last_summary_done: 0,
        }
    }

    fn on_tu_done(&mut self, abs_path: &str, seconds: f64, outcome: &TuOutcome) {
        self.done += 1;
        if self.quiet_level == 0 {
            let path = relative_to_root(abs_path, &self.repo_root);
            eprintln!(
                "{}",
                format_progress_line(self.done, self.total, seconds, &display_status_for(outcome), &path)
            );
        }
        let force = self.done >= self.total;
        self.maybe_summary(force);
    }

    /// Called once after the loop, in case the last on_tu_done() call didn't
    /// already emit the final periodic summary (e.g. tus_total == 0).
    fn finish(&mut self) {
        self.maybe_summary(true);
    }

    fn maybe_summary(&mut self, force: bool) {
        if self.quiet_level >= 2 {
            // -qq: no periodic summary at all -- only the unconditional
            // final "symhow index: N/N TUs produced content..." line
            // (printed separately, unconditionally, by run_index()) remains.
            return;
        }

        // Nothing new to report since the last summary. Checked BEFORE the
        // force bypass so finish() never re-prints an identical line
        // on_tu_done() already emitted for the exact same `done` count.
        if self.done == self.last_summary_done {
            return;
        }

        let now = Instant::now();
        let since_last = now.duration_since(self.last_summary).as_secs_f64();
        let interval_seconds = if self.quiet_level == 1 { 60.0 } else { 5.0 };
        let count_trigger = self.quiet_level == 0 && (self.done - self.last_summary_done) >= 10;
        let time_trigger = since_last >= interval_seconds;

        if !force && !count_trigger && !time_trigger {
            return;
        }

        let elapsed_seconds = now.duration_since(self.start).as_secs_f64();
        let elapsed_minutes = elapsed_seconds / 60.0;
        let tus_per_min = if elapsed_minutes > 0.0 {
            self.done as f64 / elapsed_minutes
        } else {
            0.0
        };
        let mut eta_seconds = 0.0;
        if tus_per_min > 0.0 && self.done < self.total {
            let remaining = (self.total - self.done) as f64;
            eta_seconds = (remaining / tus_per_min) * 60.0;
        }

        eprintln!(
            "{}",
            format_running_summary(self.done, self.total, tus_per_min, elapsed_seconds, eta_seconds)
        );

        self.last_summary = now;
        self.last_summary_done = self.done;
    }
}

// --- the single-writer consumer state -----------------------------------------

/// Everything the (single) consumer thread owns: the backend writer, the
/// progress reporter, the running counts, and the checkpoint/`symhow_tu`
/// bookkeeping. Deliberately the ONLY place that touches the backend.
struct ConsumerCtx<'a> {
    options: &'a IndexRunOptions,
    path_hashes: &'a HashMap<String, PathArgsInfo>,
    backend: SrctrldbBackend,
    reporter: ProgressReporter,

    tus_attempted: u32,
    tus_with_content: u32,
    tus_failed: u32,
    failed_tus: Vec<String>,

    /// One PendingTuRow per TU consumed since the last checkpoint (cleared
    /// by do_checkpoint()).
    chunk_tu_rows: Vec<PendingTuRow>,
    since_checkpoint: u32,
    checkpoint_failed: bool,

    /// Worst-so-far symhow_tu.status per path, across every entry of that
    /// path consumed so far THIS session. Persists across checkpoints -- a
    /// path whose first entry is "ok" but whose second entry (in a LATER
    /// checkpoint) is "failed" must still end up "failed".
    worst_status_by_path: HashMap<String, String>,

    /// Entries of each path still to be consumed THIS session -- the
    /// symhow_tu row for a path is only ever UPSERTed once its count hits 0,
    /// i.e. once its LAST entry (in CDB order) has actually been consumed.
    remaining_entries_by_path: HashMap<String, u32>,
}

impl<'a> ConsumerCtx<'a> {
    fn do_checkpoint(&mut self) {
        if self.checkpoint_failed || self.chunk_tu_rows.is_empty() {
            return;
        }
        if let Err(e) = self.backend.checkpoint(&self.chunk_tu_rows) {
            eprintln!("symhow index: checkpoint failed: {}", e.0);
            self.checkpoint_failed = true;
        }
        self.chunk_tu_rows.clear();
        self.since_checkpoint = 0;
    }

    /// Consumes one TU's already-produced outcome: updates the running
    /// counts, feeds the batch to the single SrctrldbBackend writer, buffers
    /// its symhow_tu row for the next checkpoint, and reports progress.
    /// Called exactly once per entry, strictly in CDB order, from a single
    /// thread.
    fn consume_outcome(&mut self, entry: &CompileEntry, outcome: &TuOutcome) {
        self.tus_attempted += 1;
        if outcome.has_content {
            self.tus_with_content += 1;
        } else {
            self.tus_failed += 1;
            self.failed_tus.push(entry.file.clone());
        }

        let consume_start = Instant::now();
        if let Err(e) = self.backend.consume(&outcome.batch) {
            eprintln!("symhow index: failed to write facts for '{}': {}", entry.file, e.0);
        }
        let consume_seconds = consume_start.elapsed().as_secs_f64();

        // Combined, order-independent per-path hash/entry_count -- so
        // whichever entry ends up UPSERTing symhow_tu writes the SAME
        // args_hash. Status is the worst seen so far for this path. The row
        // itself is only pushed into chunk_tu_rows once THIS path's LAST
        // entry is consumed -- an interrupted run that stops mid-path leaves
        // no row for it at all.
        let path_info = self
            .path_hashes
            .get(&entry.file)
            .expect("entry path missing from path_hashes");
        let new_worst = {
            let existing = self
                .worst_status_by_path
                .get(&entry.file)
                .cloned()
                .unwrap_or_default();
            if existing.is_empty() {
                outcome.status.clone()
            } else {
                worse_status(&existing, &outcome.status).to_string()
            }
        };
        self.worst_status_by_path.insert(entry.file.clone(), new_worst.clone());
        let remaining = match self.remaining_entries_by_path.get_mut(&entry.file) {
            Some(r) => {
                *r -= 1;
                *r
            }
            None => 0,
        };
        if remaining == 0 {
            self.chunk_tu_rows.push(PendingTuRow {
                path: entry.file.clone(),
                args_hash: path_info.combined_hash.clone(),
                status: new_worst,
                entry_count: path_info.entry_count,
            });
        }
        self.since_checkpoint += 1;
        if self.options.commit_every > 0
            && self.since_checkpoint >= self.options.commit_every as u32
        {
            self.do_checkpoint();
        }

        self.reporter.on_tu_done(
            &outcome.batch.stats.tu_file,
            outcome.extract_seconds + consume_seconds,
            outcome,
        );
    }

    /// True once either a real SIGINT/SIGTERM was observed, or --stop-after's
    /// TU count was reached -- checked once per consumed TU, from the single
    /// consumer thread only.
    fn interrupt_requested(&self) -> bool {
        G_INTERRUPT_REQUESTED.load(Ordering::Relaxed)
            || (self.options.stop_after >= 0 && self.tus_attempted >= self.options.stop_after as u32)
    }
}

/// Runs the full pipeline. Returns the process exit code: 0 if at least one
/// TU produced content, 5 if none did, 1 on a hard error (e.g. can't open
/// the compile database, can't open the output database), 2 if the output
/// path exists without --force (helpfully distinguishing "an interrupted
/// index is sitting there -- run `symhow update` or pass --force"), 130 if
/// interrupted by SIGINT/SIGTERM (or --stop-after) -- completed work (up to
/// the last checkpoint) is saved, and `symhow update -d <output>` resumes.
pub fn run_index(options: &IndexRunOptions, summary_out: Option<&mut IndexSummary>) -> i32 {
    install_interrupt_handlers();

    if !options.append_to_existing && std::path::Path::new(&options.output_path).exists() {
        if !options.force {
            // An existing output left with symhow_index_in_progress="1" is an
            // interrupted `symhow index` run -- point at the actual recovery
            // path (`symhow update`) rather than only offering --force.
            let mut in_progress = false;
            if let Ok(existing) = Database::open(options.output_path.as_str(), 0) {
                in_progress = get_meta(existing.conn(), "symhow_index_in_progress")
                    .unwrap_or_default()
                    == "1";
            }
            if in_progress {
                eprintln!(
                    "symhow index: output '{}' contains an interrupted index run -- finish it with `symhow update -d {}`, or pass --force to discard it and start over",
                    options.output_path, options.output_path
                );
                return 2;
            }
            eprintln!(
                "symhow index: output '{}' already exists (use --force to overwrite)",
                options.output_path
            );
            return 1;
        }
        let _ = std::fs::remove_file(&options.output_path);
        let _ = std::fs::remove_file(format!("{}-journal", options.output_path));
        let mut proj = std::path::PathBuf::from(&options.output_path);
        proj.set_extension("srctrlprj");
        let _ = std::fs::remove_file(&proj);
    }

    let mut entries: Vec<CompileEntry> = if let Some(preset) = &options.preset_entries {
        preset.clone()
    } else {
        match load(&options.compile_commands_path, &options.db_options) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("symhow index: {}", e.0);
                return 1;
            }
        }
    };

    if options.limit >= 0 && (options.limit as usize) < entries.len() {
        entries.truncate(options.limit as usize);
    }

    // Real compile_commands.json files can list the same `file` path more
    // than once (e.g. compiled for several targets with different flags) --
    // symhow_tu is keyed by path, so every entry of a duplicated path must
    // agree on one combined, order-independent args_hash/entry_count rather
    // than whichever entry happened to be consumed last. Computed once, up
    // front, over the final `entries` list (post limit).
    let path_hashes = path_args_hashes(&entries);

    let mut backend = SrctrldbBackend::new();
    backend.set_no_vacuum(options.no_vacuum);
    backend.set_verbose(options.verbose);

    if let Err(e) = backend.begin(&options.output_path) {
        eprintln!(
            "symhow index: failed to open output database '{}': {}",
            options.output_path, e.0
        );
        return 1;
    }

    // Provenance (+ symhow_index_in_progress="1") up front: written via its
    // own tiny WritableDb transaction, BEFORE the writer's first transaction
    // -- so it's durable even if this run is interrupted before a single TU
    // is consumed.
    if let Err(e) = write_provenance_early(&options.output_path, options.provenance.as_ref()) {
        eprintln!("symhow index: failed to write provenance: {}", e.0);
        return 1;
    }

    if let Err(e) = backend.start_transaction() {
        eprintln!("symhow index: failed to start writer transaction: {}", e.0);
        return 1;
    }

    let file_reg_options = FileRegistryOptions {
        include_globs: options.db_options.include_globs.clone(),
        exclude_globs: options.db_options.exclude_globs.clone(),
        system_file_rows: options.system_file_rows,
    };

    let start_time = Instant::now();

    let tus_total = entries.len() as u32;
    let quiet_level = if options.super_quiet {
        2
    } else if options.quiet {
        1
    } else {
        0
    };
    let repo_root = options
        .provenance
        .as_ref()
        .map(|p| p.repo_root.clone())
        .unwrap_or_default();

    let mut ctx = ConsumerCtx {
        options,
        path_hashes: &path_hashes,
        backend,
        reporter: ProgressReporter::new(tus_total, quiet_level, repo_root),
        tus_attempted: 0,
        tus_with_content: 0,
        tus_failed: 0,
        failed_tus: Vec::new(),
        chunk_tu_rows: Vec::new(),
        since_checkpoint: 0,
        checkpoint_failed: false,
        worst_status_by_path: HashMap::with_capacity(path_hashes.len()),
        remaining_entries_by_path: path_hashes
            .iter()
            .map(|(path, info)| (path.clone(), info.entry_count))
            .collect(),
    };

    let mut interrupted = false;

    let hw_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let num_threads: usize = if options.parallel == 0 {
        hw_threads.max(1)
    } else {
        (options.parallel.max(1)) as usize
    };

    if num_threads <= 1 || entries.len() <= 1 {
        // Sequential path: process each TU, then immediately consume it.
        // Checked after each consume(): with no worker threads in flight,
        // this gives an EXACT interruption point.
        for entry in &entries {
            let outcome = process_entry(entry, options, &file_reg_options);
            ctx.consume_outcome(entry, &outcome);
            if ctx.checkpoint_failed {
                break;
            }
            if ctx.interrupt_requested() {
                interrupted = true;
                break;
            }
        }
    } else {
        // Parallel path: a fixed pool of `num_threads` workers pulls entry
        // indices (in CDB order) off the dispatch channel and runs the
        // full, self-contained process_entry() extraction for each. The MAIN
        // thread is the sole consumer: it drains results strictly in CDB
        // order (a reorder buffer), so every observable side effect of
        // indexing -- node ids, symhow_tu rows, collision-merge outcomes,
        // progress lines -- is identical to the sequential path above,
        // regardless of num_threads. Workers are blocked from racing more
        // than 2*num_threads entries ahead of the consumer (bounded memory).
        let num_entries = entries.len();
        let window_cap = 2 * num_threads;

        // The channels live OUTSIDE std::thread::scope (spawned threads may
        // only borrow data that outlives the scope); the sender handles are
        // moved into the scope body via Option::take so the body can close
        // the dispatch channel (which unblocks the workers for the join).
        let (dispatch_tx_outer, dispatch_rx) = mpsc::channel::<usize>();
        let dispatch_rx = Mutex::new(dispatch_rx);
        let (result_tx_outer, result_rx) = mpsc::channel::<(usize, TuOutcome)>();
        let mut result_tx_opt = Some(result_tx_outer);
        let mut dispatch_tx_opt = Some(dispatch_tx_outer);

        std::thread::scope(|scope| {
            let dispatch_tx = dispatch_tx_opt.take().unwrap();
            let result_tx = result_tx_opt.take().unwrap();

            for _ in 0..num_threads {
                let rx = &dispatch_rx;
                let tx = result_tx.clone();
                let entries_ref = &entries;
                let options_ref = options;
                let file_reg_ref = &file_reg_options;
                scope.spawn(move || {
                    // clang-sys in `runtime` mode keeps the dlopen'd
                    // libclang in THREAD-LOCAL storage: every worker thread
                    // must load it once (parse_tu does this too).
                    ensure_libclang_loaded();
                    loop {
                        let idx = {
                            let guard = rx.lock().unwrap();
                            guard.recv()
                        };
                        let Ok(idx) = idx else {
                            break;
                        };
                        let outcome = process_entry(&entries_ref[idx], options_ref, file_reg_ref);
                        if tx.send((idx, outcome)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(result_tx);

            // Initial dispatch: at most window_cap entries in flight.
            let mut dispatched: usize = 0;
            while dispatched < num_entries && dispatched < window_cap {
                if dispatch_tx.send(dispatched).is_err() {
                    break;
                }
                dispatched += 1;
            }

            let mut buffer: HashMap<usize, TuOutcome> = HashMap::new();
            let mut i: usize = 0;
            while i < num_entries {
                if interrupted && i >= dispatched {
                    // Dispatch was halted before this index was ever handed
                    // to a worker -- it will never become ready.
                    break;
                }
                let outcome = loop {
                    if let Some(outcome) = buffer.remove(&i) {
                        break outcome;
                    }
                    match result_rx.recv() {
                        Ok((idx, outcome)) if idx == i => break outcome,
                        Ok((idx, outcome)) => {
                            buffer.insert(idx, outcome);
                        }
                        Err(_) => {
                            // All senders dropped before idx arrived: cannot
                            // happen for an index that was dispatched.
                            break TuOutcome::default();
                        }
                    }
                };
                ctx.consume_outcome(&entries[i], &outcome);
                // Keep the in-flight window full.
                while !interrupted && dispatched < num_entries && dispatched - (i + 1) < window_cap {
                    if dispatch_tx.send(dispatched).is_err() {
                        break;
                    }
                    dispatched += 1;
                }
                if !interrupted && (ctx.checkpoint_failed || ctx.interrupt_requested()) {
                    // A checkpoint failure is a hard error, not a clean
                    // interrupt; on interrupt, keep consuming every entry
                    // that was ALREADY dispatched (i >= dispatched breaks
                    // above), then stop.
                    interrupted = !ctx.checkpoint_failed;
                    if ctx.checkpoint_failed {
                        break;
                    }
                }
                i += 1;
            }

            // Closing the dispatch channel (all senders dropped) unblocks
            // the workers; the scope joins them.
            drop(dispatch_tx);
        });
    }

    ctx.reporter.finish();

    if ctx.checkpoint_failed {
        eprintln!("symhow index: aborting after checkpoint failure");
        return 1;
    }

    if interrupted {
        if let Err(e) = ctx.backend.finish_interrupted(&ctx.chunk_tu_rows) {
            eprintln!("symhow index: failed to checkpoint on interrupt: {}", e.0);
            return 1;
        }
        eprintln!(
            "symhow index: interrupted after {}/{} TUs -- completed work is saved; resume with: symhow update -d {}",
            ctx.tus_attempted, tus_total, options.output_path
        );
        return 130;
    }

    // Final checkpoint: covers whatever TUs weren't checkpointed yet -- for
    // commit_every=0 this is the ONLY checkpoint() call, covering the entire
    // run.
    if let Err(e) = ctx.backend.checkpoint(&ctx.chunk_tu_rows) {
        eprintln!("symhow index: final checkpoint failed: {}", e.0);
        return 1;
    }
    ctx.chunk_tu_rows.clear();

    let mut summary = IndexSummary::default();
    if let Err(e) = ctx.backend.finish(&mut summary) {
        eprintln!("symhow index: failed to finalize output database: {}", e.0);
        return 1;
    }

    summary.tus_total = tus_total;
    summary.tus_attempted = ctx.tus_attempted;
    summary.tus_with_content = ctx.tus_with_content;
    summary.tus_failed = ctx.tus_failed;
    summary.failed_tus = std::mem::take(&mut ctx.failed_tus);
    summary.elapsed_ms = millis_since(start_time);

    eprintln!(
        "symhow index: {}/{} TUs produced content ({} failed), {} symbols, {} references, {} files, {} errors, {} name collisions, {} ms",
        summary.tus_with_content,
        summary.tus_total,
        summary.tus_failed,
        summary.symbols,
        summary.references,
        summary.files,
        summary.errors,
        summary.name_collisions,
        summary.elapsed_ms
    );

    if options.json_summary {
        // symhow.index-summary/1 -- the Phase 5 envelope convention shared by
        // every other symhow/symhow query JSON schema. The C++ IndexDriver
        // itself owns this envelope (built right here in runIndex with
        // nlohmann::json and printed to stdout), so the Rust port keeps it
        // here too.
        let j = serde_json::json!({
            "symhow_version": options.symhow_version,
            "schema": "symhow.index-summary/1",
            "database": options.output_path,
            "counts": {
                "tus_total": summary.tus_total,
                "tus_attempted": summary.tus_attempted,
                "tus_with_content": summary.tus_with_content,
                "tus_failed": summary.tus_failed,
                "symbols": summary.symbols,
                "references": summary.references,
                "files": summary.files,
                "errors": summary.errors,
                "name_collisions": summary.name_collisions,
            },
            "elapsed_ms": summary.elapsed_ms,
            "failed_tus": summary.failed_tus,
        });
        println!("{}", serde_json::to_string(&j).unwrap_or_default());
    }

    let exit_code = if summary.tus_with_content > 0 { 0 } else { 5 };

    if let Some(out) = summary_out {
        *out = summary;
    }

    exit_code
}
