//! CLI-shelling tests for the hidden `symhow dbdiff` command, ported from
//! the `symhow dbdiff` cases in test/unit/test_snapshot.cpp ("`symhow dbdiff`
//! CLI smoke -- equal databases exit 0, different exit 3"): equal exit 0,
//! different exit 3, missing exit 4, and the --json envelope with
//! `"schema":"symhow.dbdiff/1"` / `"equal":false`. Runs the REAL binary via
//! `env!("CARGO_BIN_EXE_symhow")` against databases the binary itself
//! produced from tiny hermetic compile databases.

use std::path::PathBuf;
use std::process::Command;

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("symhow_cli_dbdiff_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_tu(dir: &std::path::Path, file: &str, function: &str) -> String {
    let path = dir.join(file);
    std::fs::write(
        &path,
        format!("int {}() {{ return 1; }}\n", function),
    )
    .unwrap();
    path.to_string_lossy().to_string()
}

/// Writes a compile_commands.json for `files` (already on disk under `dir`)
/// and returns its path.
fn write_cdb(dir: &std::path::Path, files: &[String]) -> PathBuf {
    let dir_str = dir.to_string_lossy().to_string();
    let entries: Vec<serde_json::Value> = files
        .iter()
        .map(|file| {
            serde_json::json!({
                "directory": dir_str,
                "file": file,
                "arguments": ["clang++", "-x", "c++", "-std=c++17", file],
            })
        })
        .collect();
    let cdb = dir.join("compile_commands.json");
    std::fs::write(&cdb, serde_json::Value::Array(entries).to_string()).unwrap();
    cdb
}

/// Runs the real `symhow` binary and returns (exit code, stdout, stderr).
fn run_symhow(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_symhow"))
        .args(args)
        .output()
        .expect("failed to spawn symhow binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn index_db(dir: &std::path::Path, cdb: &std::path::Path, out_name: &str) -> PathBuf {
    let out = dir.join(out_name);
    let (code, _stdout, stderr) = run_symhow(&[
        "index",
        "-c",
        &cdb.to_string_lossy(),
        "-o",
        &out.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "symhow index failed: {}", stderr);
    out
}

/// Two databases indexed from DIFFERENT tiny CDBs (different function
/// names) must differ in at least the node category; the same database
/// against itself must be equal.
#[test]
fn cli_dbdiff_exit_codes() {
    let dir = fresh_dir("main");
    let a_dir = dir.join("a");
    let b_dir = dir.join("b");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();

    let tu_a = write_tu(&a_dir, "a.cpp", "alpha_one");
    let cdb_a = write_cdb(&a_dir, &[tu_a]);
    let db_a = index_db(&dir, &cdb_a, "a.srctrldb");

    let tu_b = write_tu(&b_dir, "b.cpp", "beta_two");
    let cdb_b = write_cdb(&b_dir, &[tu_b]);
    let db_b = index_db(&dir, &cdb_b, "b.srctrldb");

    let db_a_str = db_a.to_string_lossy().to_string();
    let db_b_str = db_b.to_string_lossy().to_string();

    // Equal databases exit 0.
    {
        let (code, stdout, _stderr) = run_symhow(&["dbdiff", &db_a_str, &db_a_str]);
        assert_eq!(code, 0, "self-diff should exit 0, stdout: {}", stdout);
        assert!(
            stdout.contains("are logically equivalent"),
            "equal text output expected, got: {}",
            stdout
        );
    }

    // Different databases exit 3 with non-empty stdout (different function
    // names mean different node sets).
    {
        let (code, stdout, _stderr) = run_symhow(&["dbdiff", &db_a_str, &db_b_str]);
        assert_eq!(code, 3, "diff should exit 3, stdout: {}", stdout);
        assert!(!stdout.is_empty());
    }

    // A database that can't be opened exits 4.
    {
        let missing = dir.join("nonexistent.srctrldb");
        let (code, _stdout, stderr) = run_symhow(&["dbdiff", &db_a_str, &missing.to_string_lossy()]);
        assert_eq!(code, 4);
        assert!(stderr.contains("symhow dbdiff: error:"), "stderr: {}", stderr);
    }

    // --json envelope: schema symhow.dbdiff/1 with "equal":false, exit 3.
    {
        let (code, stdout, _stderr) = run_symhow(&["dbdiff", &db_a_str, &db_b_str, "--json"]);
        assert_eq!(code, 3);
        assert!(
            stdout.contains("\"schema\":\"symhow.dbdiff/1\""),
            "stdout: {}",
            stdout
        );
        assert!(stdout.contains("\"equal\":false"), "stdout: {}", stdout);

        let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
        assert_eq!(j["schema"], "symhow.dbdiff/1");
        assert_eq!(j["equal"], false);
        assert_eq!(j["symhow_version"], "0.1.0");
    }

    // --json on the equal case: "equal":true, exit 0.
    {
        let (code, stdout, _stderr) = run_symhow(&["dbdiff", &db_a_str, &db_a_str, "--json"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("\"equal\":true"), "stdout: {}", stdout);
        let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
        assert_eq!(j["equal"], true);
        assert_eq!(j["counts"]["only_in_a"], 0);
        assert_eq!(j["counts"]["only_in_b"], 0);
    }
}

/// Wrong positional count is a usage error (exit 4), and unknown flags are
/// rejected with exit 4 -- both per Cli.cpp's runDbDiff().
#[test]
fn cli_dbdiff_usage_errors() {
    let dir = fresh_dir("usage");
    let (code, _stdout, stderr) = run_symhow(&["dbdiff"]);
    assert_eq!(code, 4);
    assert!(
        stderr.contains("usage: symhow dbdiff A B"),
        "stderr: {}",
        stderr
    );

    let (code, _stdout, stderr) = run_symhow(&["dbdiff", &dir.to_string_lossy(), "--bogus"]);
    assert_eq!(code, 4);
    assert!(
        stderr.contains("unknown option '--bogus'"),
        "stderr: {}",
        stderr
    );
}
