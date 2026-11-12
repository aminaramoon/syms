//! End-to-end CLI scenario tests. Each drives the real `symhow` binary through
//! a multi-step script under tests/scenarios/ (CMake `-P` scripts and a
//! Python script); scratch state lives in $CARGO_TARGET_TMPDIR/scenarios/.
//!
//! - update_scenario: a real git history of the synthetic fixture
//!   (edit/edit-header/delete/add/rename/flag-change/uncommitted edits);
//!   after every step `symhow update` must match a fresh `symhow index`.
//! - notes_cross_worktree: notes cache identical across two clones.
//! - portable_paths_cli: relocation + `--rebind` regression.
//! - cucs_update_integration (ignored): `symhow update` over a real cucs
//!   commit range; needs SYMHOW_CUCS_DIR (SYMHOW_CUCS_UPDATE_FROM optional).
//!
//! The CMake scripts exit 77 when a required tool (git/sqlite3) is missing;
//! that is reported as a skip, not a failure.

#[path = "support/fixtures.rs"]
mod fixtures;

use std::path::{Path, PathBuf};
use std::process::Command;

fn which(tool: &str) -> String {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(tool))
                .find(|candidate| candidate.is_file())
        })
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn work_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scenarios").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    dir
}

fn script(name: &str) -> PathBuf {
    fixtures::tests_dir().join("scenarios").join(name)
}

/// Runs `cmake -D<k>=<v>... -P <script>`; panics with its output on failure.
fn run_cmake_scenario(name: &str, defines: &[(&str, String)]) {
    let mut cmd = Command::new("cmake");
    cmd.arg(format!("-DSYMHOW={}", fixtures::symhow_bin()));
    cmd.arg(format!("-DWORK={}", work_dir(name).display()));
    for (key, value) in defines {
        cmd.arg(format!("-D{key}={value}"));
    }
    cmd.arg("-P").arg(script(&format!("{name}.cmake")));
    let out = cmd.output().unwrap_or_else(|e| panic!("running cmake for {name}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(77) {
        eprintln!("{name}: skipped (required tool missing)\n{stdout}{stderr}");
        return;
    }
    assert!(out.status.success(), "{name} failed ({})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}", out.status);
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn update_scenario() {
    run_cmake_scenario(
        "update_scenario",
        &[
            ("SRC", path_str(&fixtures::synthetic_src())),
            ("GIT", which("git")),
            ("SQLITE3", which("sqlite3")),
            ("SCRIPTS", path_str(&fixtures::scripts_dir())),
        ],
    );
}

#[test]
fn notes_cross_worktree() {
    run_cmake_scenario(
        "notes_cross_worktree",
        &[
            ("SRC", path_str(&fixtures::synthetic_src())),
            ("GIT", which("git")),
            ("SCRIPTS", path_str(&fixtures::scripts_dir())),
        ],
    );
}

#[test]
fn portable_paths_cli() {
    let out = Command::new("python3")
        .arg(script("portable_paths.py"))
        .arg(fixtures::symhow_bin())
        .output()
        .expect("running python3 portable_paths.py");
    assert!(
        out.status.success(),
        "portable_paths.py failed ({})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "needs the real cucs corpus (SYMHOW_CUCS_DIR); takes several minutes"]
fn cucs_update_integration() {
    let Some(cucs) = std::env::var("SYMHOW_CUCS_DIR").ok().filter(|d| Path::new(d).is_dir()) else {
        eprintln!("cucs_update_integration: SYMHOW_CUCS_DIR not set or missing -- skipping");
        return;
    };
    run_cmake_scenario(
        "cucs_update_scenario",
        &[
            ("CUCS", cucs),
            ("GIT", which("git")),
            ("SQLITE3", which("sqlite3")),
            ("SCRIPTS", path_str(&fixtures::scripts_dir())),
            ("FROM", std::env::var("SYMHOW_CUCS_UPDATE_FROM").unwrap_or_default()),
        ],
    );
}
