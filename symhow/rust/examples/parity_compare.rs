//! parity_compare — logical (id-independent) comparison of two Sourcetrail-
//! schema databases, built on the same `take_snapshot`/`diff_snapshots` core
//! as the hidden `symhow dbdiff` CLI command. Used by the C++/Rust parity
//! harness: index the same compile_commands.json with the C++ binary and the
//! Rust binary, then run this against the two resulting .srctrldb files.
//!
//! Usage:
//!   cargo run --example parity_compare -- <a.srctrldb> <b.srctrldb> [--only CATEGORY ...] [--ignore-shared-header-locations]
//!
//! Exit code 0 = logically equal, 3 = different, 4 = a database failed to open.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: parity_compare <a.srctrldb> <b.srctrldb> [--only CATEGORY ...] [--ignore-shared-header-locations]");
        return ExitCode::from(4);
    }

    let mut only: Vec<String> = Vec::new();
    let mut ignore_shared = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--only" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") {
                    only.push(args[i].clone());
                    i += 1;
                }
            }
            "--ignore-shared-header-locations" => {
                ignore_shared = true;
                i += 1;
            }
            _ => {
                paths.push(args[i].clone());
                i += 1;
            }
        }
    }

    if paths.len() != 2 {
        eprintln!("error: exactly two database paths required");
        return ExitCode::from(4);
    }

    let options = symhow::update::SnapshotOptions { ignore_shared_header_locations: ignore_shared };

    let a = match symhow::update::take_snapshot(&paths[0], &options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}: {}", paths[0], e);
            return ExitCode::from(4);
        }
    };
    let b = match symhow::update::take_snapshot(&paths[1], &options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}: {}", paths[1], e);
            return ExitCode::from(4);
        }
    };

    let diff = symhow::update::diff_snapshots(&a, &b, &only);

    let mut equal = true;
    for entry in &diff.only_in_a {
        println!("- {}", entry);
        equal = false;
    }
    for entry in &diff.only_in_b {
        println!("+ {}", entry);
        equal = false;
    }

    println!("equal: {}", equal);
    if equal {
        ExitCode::from(0)
    } else {
        ExitCode::from(3)
    }
}