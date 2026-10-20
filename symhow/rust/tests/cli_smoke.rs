//! CLI-shelling smoke tests for the `symhow` binary, mirroring the C++ CLI
//! test steps that shell out to SYMHOW_BINARY across the suite
//! (test_resume_fixture.cpp's `symhow index`/`symhow query stats` steps,
//! test_notes_markdown.cpp's `symhow notes lint` steps): `--version`,
//! `--help`, a real index of a tiny CDB, one `query --json` verb, and
//! `notes lint` basics. Runs the REAL binary via `env!("CARGO_BIN_EXE_symhow")`.

use std::path::PathBuf;
use std::process::Command;

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("symhow_cli_smoke_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// `symhow --version`: exact C++ shape
/// "symhow <ver>, libclang <ver>, sqlite <ver>, srctrldb v4.db25.p1", exit 0,
/// on stdout.
#[test]
fn cli_version() {
    let (code, stdout, stderr) = run_symhow(&["--version"]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "stderr: {}", stderr);
    assert!(
        stdout.starts_with("symhow 0.1.0, libclang "),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains(", sqlite "), "stdout: {}", stdout);
    assert!(
        stdout.trim_end().ends_with(", srctrldb v4.db25.p1"),
        "stdout: {}",
        stdout
    );
    assert_eq!(stdout.lines().count(), 1);
}

/// `symhow --help` (and `-h`, and the no-args form): exit 0, usage text on
/// stdout, with every subcommand section present.
#[test]
fn cli_help() {
    for args in [&["--help"][..], &["-h"][..], &[][..]] {
        let (code, stdout, stderr) = run_symhow(args);
        assert_eq!(code, 0);
        assert!(stderr.is_empty(), "stderr: {}", stderr);
        assert!(stdout.starts_with("Usage: "), "stdout starts: {}", &stdout[..30.min(stdout.len())]);
        assert!(stdout.contains("index subcommand:"));
        assert!(stdout.contains("query subcommand:"));
        assert!(stdout.contains("update subcommand"));
        assert!(stdout.contains("notes subcommand"));
        assert!(stdout.contains("Exit codes: 0 ok, 1 usage error"));
        // The hidden dbdiff command is deliberately NOT in the usage text.
        assert!(!stdout.contains("dbdiff"));
    }
}

/// An unknown top-level option is a usage error: message + help on the right
/// streams, exit 1 (mirrors Cli.cpp's run()).
#[test]
fn cli_unknown_option() {
    let (code, stdout, stderr) = run_symhow(&["frobnicate"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("symhow: unknown option 'frobnicate'"));
    assert!(stdout.contains("Usage: "), "help goes to stdout");
}

/// `symhow index` on a tiny CDB via the real binary, then `symhow query
/// stats` against the produced .srctrldb -- the CLI-shelling steps
/// test_resume_fixture.cpp exercises against the C++ binary.
#[test]
fn cli_index_then_query_stats() {
    let dir = fresh_dir("index");
    let tu = dir.join("a.cpp");
    std::fs::write(&tu, "int smoke_one() { return 1; }\n").unwrap();
    let tu_str = tu.to_string_lossy().to_string();
    let dir_str = dir.to_string_lossy().to_string();
    let cdb = dir.join("compile_commands.json");
    std::fs::write(
        &cdb,
        serde_json::json!([{
            "directory": dir_str,
            "file": tu_str,
            "arguments": ["clang++", "-x", "c++", "-std=c++17", tu_str],
        }])
        .to_string(),
    )
    .unwrap();

    let db = dir.join("out.srctrldb");
    let db_str = db.to_string_lossy().to_string();
    let cdb_str = cdb.to_string_lossy().to_string();

    let (code, _stdout, stderr) = run_symhow(&["index", "-c", &cdb_str, "-o", &db_str]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    // The final summary line goes to stderr (stdout stays pure for --json).
    assert!(
        stderr.contains("TUs produced content"),
        "stderr: {}",
        stderr
    );
    assert!(db.is_file());

    // A second index without --force refuses with exit 1 (output exists).
    let (code, _stdout, stderr) = run_symhow(&["index", "-c", &cdb_str, "-o", &db_str]);
    assert_eq!(code, 1);
    assert!(stderr.contains("already exists"), "stderr: {}", stderr);

    // `symhow query stats` on the fresh db: exit 0, no "index incomplete"
    // notice (the run completed), stats text on stdout.
    let (code, stdout, stderr) = run_symhow(&["query", "stats", "-d", &db_str]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "stderr: {}", stderr);
    assert!(stdout.contains("storage_version: 25"), "stdout: {}", stdout);
    assert!(stdout.contains("nodes:"), "stdout: {}", stdout);
}

/// One `query --json` verb against the indexed db: `search` prints the
/// symhow.search/1 envelope (docs/JSON_SCHEMA.md); `stats` prints
/// symhow.stats/1. Both exit 0 and keep stdout pure JSON.
#[test]
fn cli_query_json_verb() {
    let dir = fresh_dir("qjson");
    let tu = dir.join("a.cpp");
    std::fs::write(&tu, "int json_target() { return 2; }\n").unwrap();
    let tu_str = tu.to_string_lossy().to_string();
    let dir_str = dir.to_string_lossy().to_string();
    let cdb = dir.join("compile_commands.json");
    std::fs::write(
        &cdb,
        serde_json::json!([{
            "directory": dir_str,
            "file": tu_str,
            "arguments": ["clang++", "-x", "c++", "-std=c++17", tu_str],
        }])
        .to_string(),
    )
    .unwrap();
    let db = dir.join("out.srctrldb");
    let db_str = db.to_string_lossy().to_string();
    let cdb_str = cdb.to_string_lossy().to_string();
    let (code, _stdout, stderr) = run_symhow(&["index", "-c", &cdb_str, "-o", &db_str]);
    assert_eq!(code, 0, "stderr: {}", stderr);

    // query search --json
    let (code, stdout, stderr) = run_symhow(&[
        "query", "search", "json_target", "--json", "-d", &db_str,
    ]);
    assert_eq!(code, 0, "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.is_empty(), "stderr: {}", stderr);
    let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON envelope");
    assert_eq!(j["schema"], "symhow.search/1");
    assert_eq!(j["symhow_version"], "0.1.0");
    assert_eq!(j["query"]["verb"], "search");
    assert_eq!(j["query"]["text"], "json_target");
    assert!(!j["hits"].as_array().unwrap().is_empty());

    // query stats --json
    let (code, stdout, stderr) = run_symhow(&["query", "stats", "--json", "-d", &db_str]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "stderr: {}", stderr);
    let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON envelope");
    assert_eq!(j["schema"], "symhow.stats/1");
}

/// `symhow notes lint` basics, mirroring test_notes_markdown.cpp's CLI
/// steps: a clean notes tree exits 0 with "0 problems"; a tree with a real
/// parse problem exits 3 and reports it; `--json` prints the
/// symhow.notes-lint/1 envelope.
#[test]
fn cli_notes_lint() {
    let dir = fresh_dir("lint");

    // Clean tree: one well-formed block, zero problems.
    let clean = dir.join("clean");
    std::fs::create_dir_all(&clean).unwrap();
    std::fs::write(
        clean.join("a.cpp.md"),
        "# a.cpp\n\
         \n\
         ## ns::smoke_one\n\
         \n\
         does one.\n\
         \n\
         ### what ?\n\
         \n\
         it does one.\n\
         \n\
         ### how ?\n\
         \n\
         carefully.\n\
         \n\
         ### signature\n\
         \n\
         ```cpp\n\
         int smoke_one()\n\
         ```\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_symhow(&["notes", "lint", "--notes-dir", &clean.to_string_lossy()]);
    assert_eq!(code, 0, "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stdout.contains("0 problems"), "stdout: {}", stdout);
    assert!(stdout.contains("1 blocks (1 filled)"), "stdout: {}", stdout);

    // Missing --notes-dir is a usage error (exit 1).
    let (code, _stdout, stderr) = run_symhow(&["notes", "lint"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("missing required option --notes-dir DIR"),
        "stderr: {}",
        stderr
    );

    // Not a directory is a usage error (exit 1).
    let (code, _stdout, stderr) = run_symhow(&[
        "notes",
        "lint",
        "--notes-dir",
        &dir.join("nope").to_string_lossy(),
    ]);
    assert_eq!(code, 1);
    assert!(stderr.contains("is not a directory"), "stderr: {}", stderr);

    // A tree with a real parse problem: a `### signature` section without
    // its ```cpp fence ("missing-signature-fence") is reported even without
    // --strict, exit 3.
    let bad = dir.join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(
        bad.join("b.cpp.md"),
        "# b.cpp\n\
         \n\
         ## ns::smoke_two\n\
         \n\
         does two.\n\
         \n\
         ### signature\n\
         \n\
         int smoke_two()\n",
    )
    .unwrap();
    let (code, stdout, _stderr) = run_symhow(&["notes", "lint", "--notes-dir", &bad.to_string_lossy()]);
    assert_eq!(code, 3);
    assert!(stdout.contains("missing-signature-fence"), "stdout: {}", stdout);

    // --json prints the symhow.notes-lint/1 envelope with the same finding.
    let (code, stdout, stderr) = run_symhow(&[
        "notes",
        "lint",
        "--notes-dir",
        &bad.to_string_lossy(),
        "--json",
    ]);
    assert_eq!(code, 3);
    assert!(stderr.is_empty(), "stderr: {}", stderr);
    assert!(
        stdout.contains("\"schema\":\"symhow.notes-lint/1\""),
        "stdout: {}",
        stdout
    );
    let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON envelope");
    assert_eq!(j["schema"], "symhow.notes-lint/1");
    assert_eq!(j["blocks"], 1);
    assert!(!j["problems"].as_array().unwrap().is_empty());
}
