//! Determinism proof: index the synthetic fixture's compile_commands.json
//! in-process via the library (`run_index`) and verify the resulting
//! database is LOGICALLY IDENTICAL to the shared fixture DB that the `symhow`
//! CLI built from the same CDB (tests/support/fixtures.rs, default options).
//!
//! Logical identity = same `take_snapshot` keys AND counts in every category
//! (the same id-independent comparison `symhow dbdiff` uses), so differing
//! sqlite rowids across the two independent index runs are fine.

#[path = "support/fixtures.rs"]
mod fixtures;

use symhow::index::arg_sanitizer::SanitizerOptions;
use symhow::index::cuda_support::CudaMode;
use symhow::index::file_registry::SystemFileRows;
use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::update::snapshot::{diff_snapshots, take_snapshot, SnapshotOptions};

fn fixture_db_path() -> String {
    fixtures::fixture_db()
}

fn fixture_cdb_path() -> String {
    fixtures::fixture_cdb()
}

/// Resolves the clang resource dir exactly like the flox hook: the parent of the include/ dir that holds
/// stddef.h under $FLOX_ENV/lib/clang/* (or $SYMHOW_RESOURCE_DIR when set).
fn resolve_resource_dir() -> String {
    if let Ok(dir) = std::env::var("SYMHOW_RESOURCE_DIR") {
        return dir;
    }
    if let Ok(flox_env) = std::env::var("FLOX_ENV") {
        if let Ok(entries) = std::fs::read_dir(format!("{}/lib/clang", flox_env)) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("include/stddef.h");
                if candidate.is_file() {
                    return entry.path().to_string_lossy().into_owned();
                }
            }
        }
    }
    String::new()
}

#[test]
fn library_index_of_fixture_is_logically_identical_to_cli_fixture() {
    let cdb = fixture_cdb_path();
    let cli_db = fixture_db_path();

    let out = std::env::temp_dir().join(format!("symhow_index_determinism_{}.srctrldb", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("srctrldb-journal"));
    let _ = std::fs::remove_file(out.with_extension("srctrlprj"));

    let mut sanitizer = SanitizerOptions::default();
    sanitizer.resource_dir = resolve_resource_dir();
    sanitizer.cuda_mode = CudaMode::Auto;

    let options = IndexRunOptions {
        compile_commands_path: cdb,
        output_path: out.to_string_lossy().into_owned(),
        force: true,
        quiet: true,
        super_quiet: true,
        symhow_version: "parity-test".to_string(),
        sanitizer_options: sanitizer,
        system_file_rows: SystemFileRows::Direct,
        ..Default::default()
    };

    let exit = run_index(&options, None);
    assert_eq!(exit, 0, "rust run_index failed on the fixture CDB");

    let cli = take_snapshot(&cli_db, &SnapshotOptions::default()).expect("snapshot of CLI-built fixture DB");
    let lib = take_snapshot(out.to_str().unwrap(), &SnapshotOptions::default()).expect("snapshot of library-indexed DB");

    let diff = diff_snapshots(&cli, &lib, &[]);

    // Print the divergence for debugging, then fail with a tight assertion.
    let a: Vec<&String> = diff.only_in_a.iter().collect();
    let b: Vec<&String> = diff.only_in_b.iter().collect();
    if !a.is_empty() || !b.is_empty() {
        for entry in a.iter().take(40) {
            eprintln!("- {}", entry);
        }
        for entry in b.iter().take(40) {
            eprintln!("+ {}", entry);
        }
    }
    assert!(
        diff.only_in_a.is_empty() && diff.only_in_b.is_empty(),
        "determinism divergence: {} keys only in the CLI-built fixture, {} only in the library-indexed DB",
        diff.only_in_a.len(),
        diff.only_in_b.len()
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("srctrlprj"));
}