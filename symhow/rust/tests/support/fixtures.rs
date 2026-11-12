//! Shared test fixture databases, built on demand by this crate's own
//! `symhow` binary (there is no separate C++ build producing them anymore).
//!
//! - `fixture_db()` / `fixture_cdb()`: the synthetic C/C++ project under
//!   `tests/fixtures/synthetic`. Its `compile_commands.json` comes from a
//!   configure-only CMake run (never built), then `symhow index` indexes it.
//! - `cuda_fixture_db()`: the hermetic CUDA fixture under `tests/fixtures/cuda`
//!   (hand-written nvcc-shaped CDB, forced `--cuda-mode nowrapper` with the
//!   `fake_cuda.h` stub -- no CUDA toolkit needed).
//!
//! Outputs live in `$CARGO_TARGET_TMPDIR/symhow-fixtures/` and are rebuilt
//! whenever the `symhow` binary or any fixture source changes. A file lock
//! makes concurrent test threads/binaries build them exactly once.
//! `SYMHOW_FIXTURE_DB`, `SYMHOW_FIXTURE_CDB` and `SYMHOW_CUDA_FIXTURE_DB`
//! still override the generated paths.
#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `rust/tests` -- the test-source directory (fixtures/, data/, scenarios/).
pub fn tests_dir() -> PathBuf {
    crate_dir().join("tests")
}

pub fn synthetic_src() -> PathBuf {
    tests_dir().join("fixtures/synthetic")
}

pub fn cuda_src() -> PathBuf {
    tests_dir().join("fixtures/cuda")
}

/// `rust/scripts` (symhow-notes-fake.py).
pub fn scripts_dir() -> PathBuf {
    crate_dir().join("scripts")
}

pub fn symhow_bin() -> &'static str {
    env!("CARGO_BIN_EXE_symhow")
}

pub fn fixture_db() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| built().db.clone())
}

pub fn fixture_cdb() -> String {
    std::env::var("SYMHOW_FIXTURE_CDB").unwrap_or_else(|_| built().cdb.clone())
}

pub fn cuda_fixture_db() -> String {
    std::env::var("SYMHOW_CUDA_FIXTURE_DB").unwrap_or_else(|_| built().cuda_db.clone())
}

struct Built {
    db: String,
    cdb: String,
    cuda_db: String,
}

fn built() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| build().unwrap_or_else(|e| panic!("building symhow test fixtures: {e}")))
}

fn hash_tree(dir: &Path, hasher: &mut DefaultHasher) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        path.strip_prefix(dir).unwrap().hash(hasher);
        if path.is_dir() {
            hash_tree(&path, hasher)?;
        } else {
            fs::read(&path)?.hash(hasher);
        }
    }
    Ok(())
}

fn stamp() -> std::io::Result<String> {
    let mut hasher = DefaultHasher::new();
    hash_tree(&tests_dir().join("fixtures"), &mut hasher)?;
    let meta = fs::metadata(symhow_bin())?;
    meta.len().hash(&mut hasher);
    meta.modified()?.hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd:?} exited {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn build() -> Result<Built, String> {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("symhow-fixtures");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let built = Built {
        db: root.join("fixture.srctrldb").to_string_lossy().into_owned(),
        cdb: root.join("fixture_build/compile_commands.json").to_string_lossy().into_owned(),
        cuda_db: root.join("cuda_fixture.srctrldb").to_string_lossy().into_owned(),
    };

    let lock = fs::File::create(root.join(".lock")).map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?; // released when `lock` drops

    let wanted = stamp().map_err(|e| e.to_string())?;
    let stamp_path = root.join("stamp");
    if fs::read_to_string(&stamp_path).ok().as_deref() == Some(wanted.as_str()) {
        return Ok(built);
    }
    let _ = fs::remove_file(&stamp_path);

    // Synthetic fixture: configure-only CMake run for compile_commands.json.
    let build_dir = root.join("fixture_build");
    let _ = fs::remove_dir_all(&build_dir);
    run(Command::new("cmake")
        .arg("-S")
        .arg(synthetic_src())
        .arg("-B")
        .arg(&build_dir)
        .args(["-G", "Ninja", "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON"]))?;
    run(Command::new(symhow_bin())
        .args(["index", "-c", &built.cdb, "-o", &built.db, "--force", "-q", "-q"]))?;

    // CUDA fixture: hand-written nvcc command line from the template.
    let cuda_build = root.join("cuda_fixture");
    fs::create_dir_all(&cuda_build).map_err(|e| e.to_string())?;
    let cu_file = cuda_src().join("kernels.cu");
    let template = fs::read_to_string(cuda_src().join("compile_commands.json.in")).map_err(|e| e.to_string())?;
    let cuda_cdb = cuda_build.join("compile_commands.json");
    fs::write(
        &cuda_cdb,
        template
            .replace("@SYMHOW_CUDA_FIXTURE_BUILD_DIR@", &cuda_build.to_string_lossy())
            .replace("@SYMHOW_CUDA_FIXTURE_CU_FILE@", &cu_file.to_string_lossy())
            .replace("@SYMHOW_CUDA_FIXTURE_OUTPUT@", &cuda_build.join("kernels.cu.o").to_string_lossy()),
    )
    .map_err(|e| e.to_string())?;
    let fake_cuda = cuda_src().join("fake_cuda.h");
    run(Command::new(symhow_bin())
        .args(["index", "--cuda-mode", "nowrapper", "-c"])
        .arg(&cuda_cdb)
        .args(["-o", &built.cuda_db, "--force", "-q", "-q", "--extra-arg", "-include", "--extra-arg"])
        .arg(&fake_cuda))?;

    fs::write(&stamp_path, wanted).map_err(|e| e.to_string())?;
    Ok(built)
}
