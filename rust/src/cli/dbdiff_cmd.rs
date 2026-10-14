//! `symhow dbdiff A B [--json] [--limit N] [--ignore CATEGORY]` -- a HIDDEN
//! maintenance/equivalence-testing command (deliberately not listed in
//! print_help()) built on top of `symhow::update::snapshot::{take_snapshot,
//! diff_snapshots}`. Not part of `symhow update` itself: this is the tool
//! Phase 2/3 use to verify an incrementally-updated database is logically
//! identical to a full `symhow index` re-run.
//!
//! Exit codes: 0 equal, 3 different, 4 a database couldn't be opened.
//! Mirrors Cli.cpp's runDbDiff().

use crate::cli::{cpp_atoi, symhow_version};
use crate::update::snapshot::{diff_snapshots, take_snapshot, SnapshotOptions};

pub fn run_dbdiff(args: &[String], start_idx: usize) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut want_json = false;
    let mut limit: i32 = 50;
    let mut snap_options = SnapshotOptions::default();
    let mut only_categories: Vec<String> = Vec::new();

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--json" => {
                want_json = true;
            }
            "--only" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow dbdiff: error: --only requires a value");
                    return 4;
                }
                only_categories.push(args[i + 1].clone());
                i += 1;
            }
            "--ignore" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow dbdiff: error: --ignore requires a value");
                    return 4;
                }
                let category = args[i + 1].clone();
                i += 1;
                if category == "shared-header-locations" {
                    snap_options.ignore_shared_header_locations = true;
                } else {
                    eprintln!(
                        "symhow dbdiff: error: unknown --ignore category '{}' (known: shared-header-locations)",
                        category
                    );
                    return 4;
                }
            }
            "--limit" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow dbdiff: error: --limit requires a value");
                    return 4;
                }
                limit = cpp_atoi(&args[i + 1]);
                i += 1;
            }
            _ if !arg.is_empty() && arg.starts_with('-') => {
                eprintln!("symhow dbdiff: error: unknown option '{}'", arg);
                return 4;
            }
            _ => {
                positional.push(arg.to_string());
            }
        }
        i += 1;
    }

    if positional.len() != 2 {
        eprintln!(
            "usage: symhow dbdiff A B [--json] [--limit N] [--ignore shared-header-locations] [--only CATEGORY]"
        );
        return 4;
    }

    let a = match take_snapshot(&positional[0], &snap_options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("symhow dbdiff: error: {}", e.0);
            return 4;
        }
    };
    let b = match take_snapshot(&positional[1], &snap_options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("symhow dbdiff: error: {}", e.0);
            return 4;
        }
    };

    let diff = diff_snapshots(&a, &b, &only_categories);
    let equal = diff.only_in_a.is_empty() && diff.only_in_b.is_empty();

    if want_json {
        let j = serde_json::json!({
            "symhow_version": symhow_version(),
            "schema": "symhow.dbdiff/1",
            "equal": equal,
            "counts": {
                "only_in_a": diff.only_in_a.len(),
                "only_in_b": diff.only_in_b.len(),
            },
            "only_in_a": diff.only_in_a,
            "only_in_b": diff.only_in_b,
        });
        println!("{}", j.to_string());
    } else if equal {
        println!(
            "symhow dbdiff: {} and {} are logically equivalent",
            positional[0], positional[1]
        );
    } else {
        println!(
            "symhow dbdiff: {} and {} differ ({} only in A, {} only in B)",
            positional[0],
            positional[1],
            diff.only_in_a.len(),
            diff.only_in_b.len()
        );
        let mut shown = 0;
        for s in &diff.only_in_a {
            if shown >= limit {
                break;
            }
            shown += 1;
            println!("  - only in A: {}", s);
        }
        shown = 0;
        for s in &diff.only_in_b {
            if shown >= limit {
                break;
            }
            shown += 1;
            println!("  + only in B: {}", s);
        }
    }

    if equal {
        0
    } else {
        3
    }
}
