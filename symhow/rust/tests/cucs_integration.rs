//! Real-CUCS end-to-end indexing, query, parallelism, and SIGINT/resume checks.
//! Port of the former C++ test_cucs.cpp; artifacts are kept for diagnosis.
//! Run with `cargo test --release --test cucs_integration -- --ignored --nocapture`.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use symhow::common::Result;
use symhow::db::enums::{edge_kind, node_kind};
use symhow::db::name_decode::decode_serialized_name;
use symhow::db::Database;
use symhow::query::{resolve_symbol, BfsResult, QueryEngine, ResolveOutcome};

const BINARY: &str = env!("CARGO_BIN_EXE_symhow");
const REMAP: &str = "/mnt/workspace=/home/aaramoon/workspace";

#[derive(Default)]
struct Checks {
    passed: usize,
    failures: Vec<String>,
}

impl Checks {
    fn check(&mut self, condition: bool, description: impl Into<String>) {
        let description = description.into();
        if condition {
            println!("[PASS] {description}");
            self.passed += 1;
        } else {
            self.fail(description);
        }
    }

    fn fail(&mut self, description: impl Into<String>) {
        let description = description.into();
        println!("[FAIL] {description}");
        self.failures.push(description);
    }

    fn finish(self) {
        println!(
            "cucs_integration: SUMMARY {} passed, {} failed",
            self.passed,
            self.failures.len()
        );
        assert!(
            self.failures.is_empty(),
            "CUCS integration failures:\n{}",
            self.failures.join("\n")
        );
    }
}

fn resolve_unique(
    checks: &mut Checks,
    db: &Database,
    spec: &str,
    kinds: Option<&[i32]>,
    label: &str,
) -> Option<i64> {
    match resolve_symbol(db, spec, kinds, None) {
        Ok(result) if result.outcome == ResolveOutcome::Unique => Some(result.candidates[0].id),
        Ok(result) => {
            checks.fail(format!(
                "{label}: resolve_symbol({spec:?}) expected Unique, got {:?} ({} candidates)",
                result.outcome,
                result.candidates.len()
            ));
            None
        }
        Err(error) => {
            checks.fail(format!("{label}: resolve_symbol({spec:?}): {error}"));
            None
        }
    }
}

fn non_seed_names(result: &BfsResult) -> BTreeSet<String> {
    result
        .nodes
        .iter()
        .filter(|node| node.depth > 0)
        .map(|node| decode_serialized_name(&node.serialized_name).name_path())
        .collect()
}

fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..]
        .iter()
        .map(|line| format!("    | {line}\n"))
        .collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Capture directly to files, without shell interpolation or a pipe buffer.
fn captured(
    checks: &mut Checks,
    output: &Path,
    tag: &str,
    command: &mut Command,
) -> (i32, Duration) {
    let stdout = output.join(format!("{tag}_stdout.log"));
    let stderr = output.join(format!("{tag}_stderr.log"));
    println!("cucs_integration: running: {command:?}");
    let start = Instant::now();
    let result = (|| -> std::io::Result<_> {
        command
            .stdout(File::create(stdout)?)
            .stderr(File::create(stderr)?)
            .status()
    })();
    let code = match result {
        Ok(status) => status.code().unwrap_or(-1),
        Err(error) => {
            checks.fail(format!("running {tag}: {error}"));
            -1
        }
    };
    (code, start.elapsed())
}

fn index_command(cdb: &Path, database: &Path) -> Command {
    let mut command = Command::new(BINARY);
    command
        .arg("index")
        .arg("-c")
        .arg(cdb)
        .arg("-o")
        .arg(database)
        .args(["--force", "--remap", REMAP]);
    command
}

fn compare(checks: &mut Checks, output: &Path, tag: &str, a: &Path, b: &Path, label: &str) {
    if !a.exists() || !b.exists() {
        checks.check(false, label);
        return;
    }
    let mut command = Command::new(BINARY);
    command.arg("dbdiff").arg(a).arg(b);
    let (code, _) = captured(checks, output, tag, &mut command);
    println!(
        "cucs_integration: {tag} exit code = {code}\n{}{}",
        last_lines(&read(&output.join(format!("{tag}_stdout.log"))), 20),
        last_lines(&read(&output.join(format!("{tag}_stderr.log"))), 10)
    );
    checks.check(code == 0, label);
}

fn query_checks(checks: &mut Checks, database: &Path, index_stderr: &str) -> Result<()> {
    let db = Database::open(database.to_string_lossy().into_owned(), 0)?;
    let engine = QueryEngine::new(&db)?;
    for spec in ["cucascade", "cucascade::memory"] {
        let id = resolve_unique(checks, &db, spec, Some(&[node_kind::NAMESPACE]), "[4]");
        checks.check(
            id.is_some(),
            format!("[4] '{spec}' resolves uniquely to a NAMESPACE node"),
        );
    }

    let base = resolve_unique(
        checks,
        &db,
        "cucascade::memory::reservation_limit_policy",
        None,
        "[5]",
    );
    if let Some(base) = base {
        let names = non_seed_names(&engine.derived(&[base], 1, 0, 0)?);
        let expected = ["ignore", "fail", "increase"]
            .map(|prefix| format!("cucascade::memory::{prefix}_reservation_limit_policy"));
        checks.check(expected.iter().all(|name| names.contains(name)),
            "[5] derived(reservation_limit_policy, depth=1) is a superset of {ignore_,fail_,increase_}reservation_limit_policy");
        let locations = engine.definition(&[base])?.locations;
        checks.check(
            locations.iter().any(|loc| {
                loc.file_path
                    .contains("include/cucascade/memory/memory_reservation.hpp")
                    && loc.start_line == 76
            }),
            "[5] reservation_limit_policy's own definition is at memory_reservation.hpp:76",
        );
        for (prefix, line) in [("ignore", 113), ("fail", 131), ("increase", 149)] {
            let spec = format!("cucascade::memory::{prefix}_reservation_limit_policy");
            let id = resolve_unique(checks, &db, &spec, None, "[5]");
            let mut ok = false;
            if let Some(id) = id {
                ok = engine.definition(&[id])?.locations.iter().any(|loc| {
                    loc.file_path
                        .contains("include/cucascade/memory/memory_reservation.hpp")
                        && loc.start_line == line
                });
            }
            checks.check(
                ok,
                format!("[5] {spec}'s definition is at memory_reservation.hpp:{line}"),
            );
        }
    } else {
        checks.check(
            false,
            "[5] derived(reservation_limit_policy, depth=1) superset check",
        );
    }

    let id = resolve_unique(
        checks,
        &db,
        "cucascade::memory::increase_reservation_limit_policy",
        None,
        "[6]",
    );
    let bases_ok = match id {
        Some(id) => {
            non_seed_names(&engine.bases(&[id], 1, 0, 0)?)
                == BTreeSet::from(["cucascade::memory::reservation_limit_policy".to_string()])
        }
        None => false,
    };
    checks.check(bases_ok, "[6] bases(increase_reservation_limit_policy, depth=1) == exactly {reservation_limit_policy}");

    let id = resolve_unique(
        checks,
        &db,
        "cucascade::memory::reservation_limit_policy::get_policy_name",
        None,
        "[7]",
    );
    let mut override_count = 0;
    if let Some(id) = id {
        override_count = engine
            .overridden_by(&[id], 0)?
            .edges
            .iter()
            .filter(|edge| edge.kind == edge_kind::OVERRIDE && edge.target == id)
            .count();
    }
    println!(
        "cucs_integration: [7] found {override_count} OVERRIDE edge(s) targeting get_policy_name"
    );
    checks.check(
        override_count >= 1,
        "[7] at least 1 OVERRIDE edge targets reservation_limit_policy::get_policy_name",
    );

    let id = resolve_unique(
        checks,
        &db,
        "cucascade::disk_data_representation",
        None,
        "[8]",
    );
    let contains_base = match id {
        Some(id) => non_seed_names(&engine.bases(&[id], 0, 0, 0)?)
            .contains("cucascade::idata_representation"),
        None => false,
    };
    checks.check(
        contains_base,
        "[8] bases(disk_data_representation, depth=0) contains cucascade::idata_representation",
    );

    let id = resolve_unique(checks, &db, "cucascade::data_batch::make", None, "[9]");
    let mut monotonic = false;
    if let Some(id) = id {
        let c1 = engine.callers(&[id], 1, 0, 0)?.nodes.len();
        let c2 = engine.callers(&[id], 2, 0, 0)?.nodes.len();
        let c3 = engine.callers(&[id], 3, 0, 0)?.nodes.len();
        println!("cucs_integration: [9] callers(data_batch::make) node_count at depth 1/2/3 = {c1}/{c2}/{c3}");
        monotonic = c1 <= c2 && c2 <= c3;
    }
    checks.check(
        monotonic,
        "[9] callers(data_batch::make) node_count is monotonically non-decreasing over depth 1..3",
    );

    // Preserve the original strict probes: a non-Unique CUDA symbol is a
    // failure of its own, even when the error-row branch of the XOR holds.
    let mut all_resolved = true;
    let mut all_have_call_edge = true;
    for (launcher, kernel) in [
        ("performGpuMemoryWork", "gpu_memory_work_kernel"),
        ("verifyGpuMemoryWork", "gpu_memory_verification_kernel"),
    ] {
        let launcher = resolve_unique(checks, &db, launcher, None, "[10]/probe");
        let kernel = resolve_unique(checks, &db, kernel, None, "[10]/probe");
        match (launcher, kernel) {
            (Some(launcher), Some(kernel)) => {
                if !engine
                    .callees(&[launcher], 1, 0, 0)?
                    .nodes
                    .iter()
                    .any(|node| node.id == kernel)
                {
                    all_have_call_edge = false;
                }
            }
            _ => all_resolved = false,
        }
    }
    let kernels_indexed_with_call_edge = all_resolved && all_have_call_edge;
    let mut cu_file_has_error = false;
    let mut cu_file_error_message = String::new();
    db.run(
        "SELECT e.message FROM error e
        JOIN occurrence o ON o.element_id=e.id
        JOIN source_location sl ON sl.id=o.source_location_id
        JOIN file f ON f.id=sl.file_node_id
        WHERE f.path LIKE '%test_gpu_kernels.cu'",
        [],
        |row| {
            cu_file_has_error = true;
            cu_file_error_message = row.get::<_, Option<String>>(0)?.unwrap_or_default();
            Ok(())
        },
    )?;
    println!("cucs_integration: [10] kernelsIndexedWithCallEdge={kernels_indexed_with_call_edge}, cuFileHasError={cu_file_has_error}");
    if cu_file_has_error {
        println!("cucs_integration: [10] .cu file error message: {cu_file_error_message}");
    }
    for line in index_stderr
        .lines()
        .filter(|line| line.contains("test_gpu_kernels.cu"))
    {
        println!("cucs_integration: [10] index progress line: {line}");
    }
    checks.check(kernels_indexed_with_call_edge != cu_file_has_error,
        "[10] CUDA XOR: exactly one of {kernel+launcher indexed with CALL edge, .cu file has an error row} holds");
    Ok(())
}

#[test]
#[ignore = "needs the real cucs corpus (SYMHOW_CUCS_DIR)"]
fn cucs_integration() {
    let Some(root) = std::env::var_os("SYMHOW_CUCS_DIR")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
    else {
        eprintln!(
            "cucs_integration: SYMHOW_CUCS_DIR unset -- skipping (needs the real cucs corpus)"
        );
        return;
    };
    let cdb = root.join("compile_commands.json");
    if !cdb.exists() {
        eprintln!(
            "cucs_integration: '{}' not found -- skipping (needs the real cucs corpus)",
            cdb.display()
        );
        return;
    }
    println!("cucs_integration: SYMHOW_CUCS_DIR={}", root.display());
    println!("cucs_integration: symhow binary={BINARY}");
    let mut checks = Checks::default();
    let cdb_json = match fs::read(&cdb).map_err(|e| e.to_string()).and_then(|bytes| {
        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string())
    }) {
        Ok(value) => value,
        Err(error) => {
            checks.fail(format!("parsing {} as JSON: {error}", cdb.display()));
            checks.finish();
            return;
        }
    };
    let real_tu_count = match &cdb_json {
        serde_json::Value::Array(entries) => entries.len(),
        _ => {
            checks.fail(format!(
                "{} is not a compilation-database JSON array",
                cdb.display()
            ));
            checks.finish();
            return;
        }
    };
    println!("cucs_integration: real compile_commands.json TU count = {real_tu_count}");
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cucs_integration");
    fs::create_dir_all(&output).expect("create CUCS integration output directory");
    let database = output.join("cucs.srctrldb");
    let parallel_database = output.join("cucs_j8.srctrldb");
    let mut command = index_command(&cdb, &database);
    command.args(["-v", "--json"]);
    let (code, sequential_wall) = captured(&mut checks, &output, "cucs_index", &mut command);
    let index_stdout = read(&output.join("cucs_index_stdout.log"));
    let index_stderr = read(&output.join("cucs_index_stderr.log"));
    println!(
        "cucs_integration: symhow index exit code = {code}, wall time = {:.1}s\n{}",
        sequential_wall.as_secs_f64(),
        last_lines(&index_stderr, 15)
    );
    checks.check(code == 0, "[3] symhow index process exits 0");
    checks.check(
        sequential_wall.as_secs_f64() < 3600.0,
        "[3] symhow index completes within the 3600s timeout (self-measured)",
    );
    let summary = match serde_json::from_str::<serde_json::Value>(&index_stdout) {
        Ok(summary) => Some(summary),
        Err(error) => {
            checks.fail(format!(
                "parsing symhow index --json stdout as JSON: {error}\nstdout was:\n{index_stdout}"
            ));
            None
        }
    };
    let count = |key| {
        summary
            .as_ref()
            .and_then(|s| s["counts"][key].as_i64())
            .unwrap_or(-1)
    };
    println!("cucs_integration: summary.counts = {{tus_total={}, tus_attempted={}, tus_with_content={}}}",
        count("tus_total"), count("tus_attempted"), count("tus_with_content"));
    checks.check(
        count("tus_total") == real_tu_count as i64,
        "[1] summary.counts.tus_total == real compile_commands.json entry count",
    );
    checks.check(
        count("tus_attempted") == real_tu_count as i64,
        "[1] summary.counts.tus_attempted == real compile_commands.json entry count",
    );
    checks.check(
        count("tus_with_content") >= 190,
        "[2] summary.counts.tus_with_content >= 190",
    );
    if database.exists() {
        if let Err(error) = query_checks(&mut checks, &database, &index_stderr) {
            checks.fail(format!("query-side assertions [4]-[10]: {error}"));
        }
    } else {
        for step in 4..=10 {
            checks.fail(format!(
                "[{step}] database {} was not produced; cannot run query assertions",
                database.display()
            ));
        }
        checks.finish();
        return;
    }

    let mut command = index_command(&cdb, &parallel_database);
    command.args(["-j", "8", "-q", "--json"]);
    let (parallel_code, parallel_wall) =
        captured(&mut checks, &output, "cucs_index_j8", &mut command);
    println!("cucs_integration: [11] symhow index -j 8 exit code = {parallel_code}, wall time = {:.1}s (sequential was {:.1}s, speedup {:.2}x)\n{}",
        parallel_wall.as_secs_f64(), sequential_wall.as_secs_f64(),
        sequential_wall.as_secs_f64() / parallel_wall.as_secs_f64().max(f64::MIN_POSITIVE),
        last_lines(&read(&output.join("cucs_index_j8_stderr.log")), 15));
    checks.check(parallel_code == 0, "[11] symhow index -j 8 process exits 0");
    compare(&mut checks, &output, "cucs_dbdiff_j1_vs_j8", &database, &parallel_database,
        "[11] symhow dbdiff: sequential (-j 1) and parallel (-j 8) cucs databases are logically equivalent");

    let resume_database = output.join("cucs_resume.srctrldb");
    let _ = fs::remove_file(&resume_database);
    let journal = PathBuf::from(format!("{}-journal", resume_database.display()));
    let _ = fs::remove_file(&journal);
    let mut command = index_command(&cdb, &resume_database);
    command.args(["-j", "4", "--commit-every", "5"]);
    println!("cucs_integration: running (SIGINT after 5s): {command:?}");
    let interrupted = (|| -> std::io::Result<i32> {
        let mut child = command
            .stdout(File::create(output.join("cucs_resume_index_stdout.log"))?)
            .stderr(File::create(output.join("cucs_resume_index_stderr.log"))?)
            .spawn()?;
        std::thread::sleep(Duration::from_secs(5));
        // Signal the actual symhow child, not a shell wrapper. A real SIGINT
        // exercises the production checkpoint/signal path, just as before.
        let signal_result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        if signal_result != 0 {
            eprintln!(
                "cucs_integration: sending SIGINT: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(child.wait()?.code().unwrap_or(-1))
    })();
    let resume_code = match interrupted {
        Ok(code) => code,
        Err(error) => {
            checks.fail(format!("[12] running interrupted index: {error}"));
            -1
        }
    };
    println!(
        "cucs_integration: [12] SIGINT'd symhow index exit code = {resume_code}\n{}",
        last_lines(&read(&output.join("cucs_resume_index_stderr.log")), 10)
    );
    checks.check(resume_code == 130, "[12] SIGINT'd `symhow index` exits 130");
    checks.check(
        !journal.exists(),
        "[12] no hot journal left next to the interrupted database",
    );
    let mut opened_ok = false;
    let mut interrupted_tu_count = -1_i64;
    match Database::open(resume_database.to_string_lossy().into_owned(), 0) {
        Ok(db) => {
            opened_ok = true;
            if let Err(error) = db.run("SELECT count(*) FROM symhow_tu", [], |row| {
                interrupted_tu_count = row.get(0)?;
                Ok(())
            }) {
                println!("cucs_integration: [12] failed to count interrupted TU rows: {error}");
            }
        }
        Err(error) => {
            println!("cucs_integration: [12] failed to open interrupted db read-only: {error}")
        }
    }
    checks.check(
        opened_ok,
        "[12] interrupted database opens read-only cleanly",
    );
    println!("cucs_integration: [12] symhow_tu rows after interrupt = {interrupted_tu_count} (of {real_tu_count} total CDB entries)");
    checks.check(interrupted_tu_count > 0 && interrupted_tu_count < real_tu_count as i64,
        "[12] interrupted database has SOME but not ALL symhow_tu rows (real interruption, not a completed run)");
    let mut command = Command::new(BINARY);
    command
        .arg("update")
        .arg("-d")
        .arg(&resume_database)
        .args(["-j", "8"]);
    let (update_code, update_wall) =
        captured(&mut checks, &output, "cucs_resume_update", &mut command);
    println!("cucs_integration: [13] symhow update -d (resume) exit code = {update_code}, wall time = {:.1}s\n{}",
        update_wall.as_secs_f64(), last_lines(&read(&output.join("cucs_resume_update_stderr.log")), 10));
    checks.check(
        update_code == 0,
        "[13] `symhow update` resumes and completes the interrupted index",
    );
    compare(&mut checks, &output, "cucs_dbdiff_resume_vs_j8", &resume_database, &parallel_database,
        "[13] symhow dbdiff: resumed-then-completed and fresh -j 8 cucs databases are logically equivalent");
    checks.finish();
}
