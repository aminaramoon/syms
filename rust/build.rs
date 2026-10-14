//! Build script for the `symhow` Rust backend.
//!
//! The C++ SourcetrailDB *writer* (third_party/sourcetraildb) remains C++ and
//! is compiled here into a static library behind a thin C ABI shim
//! (`ffi/symhow_srctrldb_bridge.cpp`). Everything else in symhow is pure Rust.
//!
//! The C++ side needs a C++ compiler and the SQLite dev headers/library. Both
//! come from the flox environment (`$FLOX_ENV`), which the user activates
//! before running cargo. When `$FLOX_ENV` is unset we fall back to plain
//! `c++`/system sqlite3 so an out-of-flox build can still at least configure.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=ffi/symhow_srctrldb_bridge.cpp");
    println!("cargo:rerun-if-changed=ffi/symhow_srctrldb_bridge.h");
    println!("cargo:rerun-if-changed=../third_party/sourcetraildb/src");
    println!("cargo:rerun-if-changed=../third_party/sourcetraildb/include");

    let flox_env = env::var("FLOX_ENV").ok();
    let flox_include = flox_env.as_ref().map(|e| PathBuf::from(e).join("include"));
    let flox_lib = flox_env.as_ref().map(|e| PathBuf::from(e).join("lib"));

    // Locate the sqlite3 dev headers/library. Prefer the flox env; fall back
    // to system locations (pkg-config handles the link step for rusqlite, but
    // the C++ shim compiles `#include "sqlite3.h"` itself, so it needs the
    // include dir explicitly).
    let sqlite_include = flox_include
        .as_ref()
        .filter(|p| p.join("sqlite3.h").exists())
        .cloned()
        .or_else(|| Some(PathBuf::from("/usr/include")))
        .unwrap();

    // nlohmann_json is header-only; NameHierarchy.cpp does `#include "json.hpp"`
    // (no nlohmann/ prefix), so the include path must point at the `nlohmann`
    // subdirectory.
    let nlohmann_include = flox_include
        .as_ref()
        .map(|p| p.join("nlohmann"))
        .filter(|p| p.join("json.hpp").exists());

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .flag_if_supported("-w") // vendored 2018-era code is noisy under clang 21
        .include("../third_party/sourcetraildb/include")
        .include("../third_party/sourcetraildb/cpp_sqlite")
        .include("../third_party/sourcetraildb")
        .include(&sqlite_include)
        .include("ffi");

    if let Some(nl) = &nlohmann_include {
        build.include(nl);
    }

    // The writer's own translation units.
    for src in [
        "DatabaseStorage.cpp",
        "DefinitionKind.cpp",
        "EdgeKind.cpp",
        "ElementComponentKind.cpp",
        "LocationKind.cpp",
        "NameHierarchy.cpp",
        "NodeKind.cpp",
        "ReferenceKind.cpp",
        "SourcetrailDBWriter.cpp",
        "SymbolKind.cpp",
        "utility.cpp",
    ] {
        build.file(format!("../third_party/sourcetraildb/src/{src}"));
    }
    build.file("../third_party/sourcetraildb/cpp_sqlite/CppSQLite3.cpp");

    // Our C ABI shim.
    build.file("ffi/symhow_srctrldb_bridge.cpp");

    build.compile("symhow_srctrldb");

    // Link against sqlite3 (shared by the C++ writer and rusqlite).
    if let Some(lib) = &flox_lib {
        println!("cargo:rustc-link-search=native={}", lib.display());
    }
    println!("cargo:rustc-link-lib=dylib=sqlite3");
}