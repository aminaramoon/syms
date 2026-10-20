//! THE core C++/Rust parity proof: index the synthetic fixture's
//! compile_commands.json with the RUST pipeline (`run_index`) and verify the
//! resulting database is LOGICALLY IDENTICAL to the one the C++ binary
//! built (build/fixture.srctrldb, produced by the `fixture_index` CTest
//! target with all-default options).
//!
//! Logical identity = same `take_snapshot` keys AND counts in every category
//! (the same id-independent comparison `symhow dbdiff` uses), so differing
//! sqlite rowids across the two independent index runs are fine.

use symhow::index::arg_sanitizer::SanitizerOptions;
use symhow::index::cuda_support::CudaMode;
use symhow::index::file_registry::SystemFileRows;
use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::update::snapshot::{diff_snapshots, take_snapshot, SnapshotOptions};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn fixture_cdb_path() -> String {
    std::env::var("SYMHOW_FIXTURE_CDB")
        .unwrap_or_else(|_| "../cpp/build/fixture_build/compile_commands.json".to_string())
}

/// Resolves the clang resource dir exactly like the C++ build's
/// FindLibClang.cmake / flox hook: the parent of the include/ dir that holds
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
fn rust_indexed_fixture_is_logically_identical_to_cpp_fixture() {
    let cdb = fixture_cdb_path();
    if !std::path::Path::new(&cdb).exists() {
        eprintln!(
            "skipping: fixture CDB not found at {} (build the C++ tree first: cmake --preset dev && cmake --build --preset dev)",
            cdb
        );
        return;
    }
    let cpp_db = fixture_db_path();
    if !std::path::Path::new(&cpp_db).exists() {
        panic!("fixture DB not found at {} (run ctest --preset dev at least once)", cpp_db);
    }

    let out = std::env::temp_dir().join(format!("symhow_parity_fixture_{}.srctrldb", std::process::id()));
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

    let cpp = take_snapshot(&cpp_db, &SnapshotOptions::default()).expect("snapshot of C++ fixture DB");
    let rust = take_snapshot(out.to_str().unwrap(), &SnapshotOptions::default()).expect("snapshot of Rust-indexed DB");

    let diff = diff_snapshots(&cpp, &rust, &[]);

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
        "C++/Rust parity divergence: {} keys only in C++, {} only in Rust",
        diff.only_in_a.len(),
        diff.only_in_b.len()
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(out.with_extension("srctrlprj"));
}