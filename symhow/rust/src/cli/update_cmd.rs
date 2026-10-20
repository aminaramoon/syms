//! `symhow update` (Cli.cpp's runUpdateCmd + parseUpdateFlags): flag parsing
//! into `symhow::update::update_driver::UpdateRunOptions` and dispatch to
//! `run_update`. Exit codes 0/1/2/5 come from the update driver itself.

use crate::cli::{cpp_stoi, print_help, symhow_version};
use crate::index::compile_db::parse_remap_arg;
use crate::index::cuda_support::parse_cuda_mode;
use crate::index::file_registry::parse_system_file_rows;
use crate::update::update_driver::{run_update, UpdateRunOptions};

fn print_update_usage_error(message: &str) {
    eprintln!("symhow update: {}", message);
    eprintln!("Try 'symhow --help' for usage.");
}

/// Parses `symhow update` subcommand arguments (argv[start_idx..]) into
/// UpdateRunOptions. --files is repeatable AND greedily consumes every
/// following non-flag positional too (so `--files a.cpp b.cpp` works without
/// repeating the flag), stopping at the next `-`-prefixed token. Mirrors
/// Cli.cpp's parseUpdateFlags(); returns false (having printed a usage
/// error) on any parse failure.
fn parse_update_flags(args: &[String], start_idx: usize, opts: &mut UpdateRunOptions) -> bool {
    // Exit 1 (usage error), NOT 2 -- 2 is documented as "ambiguous symbol /
    // refused (legacy db / update in progress)", a semantically different
    // failure from "you typo'd a flag".
    let require_value = |i: usize, flag: &str| -> Option<String> {
        if i + 1 >= args.len() {
            print_update_usage_error(&format!("option '{}' requires a value", flag));
            return None;
        }
        Some(args[i + 1].clone())
    };

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                opts.db_path = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "--repo" => {
                opts.repo_root = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "--to" => {
                opts.to_rev = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "--from" => {
                opts.from_rev = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "-c" | "--compile-commands" => {
                opts.cdb_path = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "--files" => {
                i += 1;
                if i >= args.len() {
                    print_update_usage_error("option '--files' requires at least one value");
                    return false;
                }
                while i < args.len() && !(args[i].len() > 1 && args[i].starts_with('-')) {
                    opts.explicit_files.push(args[i].clone());
                    i += 1;
                }
                i -= 1;
            }
            "--remap" => {
                let value = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                match parse_remap_arg(&value) {
                    Ok(pair) => opts.db_overrides.remaps.push(pair),
                    Err(e) => {
                        print_update_usage_error(&e.0);
                        return false;
                    }
                }
                opts.db_overrides_given = true;
            }
            "--include" => {
                opts.db_overrides.include_globs.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
                opts.db_overrides_given = true;
            }
            "--exclude" => {
                opts.db_overrides.exclude_globs.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
                opts.db_overrides_given = true;
            }
            "--cuda-mode" => {
                let value = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                match parse_cuda_mode(&value) {
                    Ok(mode) => opts.cuda_mode_override = mode,
                    Err(e) => {
                        print_update_usage_error(&e.0);
                        return false;
                    }
                }
                opts.cuda_mode_given = true;
            }
            "--system-file-rows" => {
                let value = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                match parse_system_file_rows(&value) {
                    Ok(mode) => opts.system_file_rows_override = mode,
                    Err(e) => {
                        print_update_usage_error(&e.0);
                        return false;
                    }
                }
                opts.system_file_rows_given = true;
            }
            "--extra-arg" => {
                opts.extra_arg_override.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
                opts.extra_arg_given = true;
            }
            "--drop-arg" => {
                opts.drop_arg_override.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
                opts.drop_arg_given = true;
            }
            "--std" => {
                opts.std_override = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                opts.std_given = true;
            }
            "--c-std" => {
                opts.c_std_override = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                opts.c_std_given = true;
            }
            "--dry-run" => {
                opts.dry_run = true;
            }
            "--exclude-submodules" => {
                opts.exclude_submodules = true;
            }
            "--in-place" => {
                opts.in_place = true;
            }
            "--parallel" | "-j" => {
                let value = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                match cpp_stoi(&value) {
                    Some(v) => opts.parallel = v,
                    None => {
                        print_update_usage_error(&format!("invalid --parallel/-j value: {}", value));
                        return false;
                    }
                }
                if opts.parallel < 0 {
                    print_update_usage_error("--parallel/-j must be >= 0 (0 = hardware_concurrency())");
                    return false;
                }
            }
            "--no-vacuum" => {
                opts.no_vacuum = true;
            }
            "--json" => {
                opts.json = true;
            }
            "-q" | "--quiet" => {
                // Quiet is already the default (run_update only prints
                // per-TU progress lines when verbose > 0) -- accepted for
                // symmetry with `symhow index`/`symhow query`, but a no-op.
            }
            "-v" | "--verbose" => {
                opts.verbose += 1;
            }
            "-h" | "--help" => {
                print_help("symhow");
                std::process::exit(0);
            }
            _ => {
                print_update_usage_error(&format!("unknown option '{}'", arg));
                return false;
            }
        }
        // Arms that consumed a value already advanced i to it; this trailing
        // step moves past it (the C++ for-loop's ++i).
        i += 1;
    }
    true
}

/// Mirrors Cli.cpp's runUpdateCmd().
pub fn run_update_cmd(args: &[String], start_idx: usize) -> i32 {
    let mut opts = UpdateRunOptions {
        symhow_version: symhow_version().to_string(),
        ..Default::default()
    };

    if !parse_update_flags(args, start_idx, &mut opts) {
        return 1;
    }

    if opts.db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            opts.db_path = env;
        }
    }
    if opts.db_path.is_empty() {
        print_update_usage_error("missing required option -d/--db PATH");
        return 1;
    }

    run_update(&opts, None)
}
