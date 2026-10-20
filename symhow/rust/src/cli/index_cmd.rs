//! `symhow index` (Cli.cpp's runIndex + buildProvenance): hand-rolled flag
//! parsing, the --dry-run sanitize-print pipeline, and the real run via
//! `symhow::index::index_driver::run_index`.

use crate::cli::{
    absolute_lexically_normal, absolute_parent, cpp_stoi, cpp_stol, print_help, symhow_version,
    trim_trailing_newline, utc_timestamp_now,
};
use crate::common::process;
use crate::index::arg_sanitizer::SanitizerOptions;
use crate::index::compile_db::{load, parse_remap_arg, CompileDbOptions};
use crate::index::cuda_support::parse_cuda_mode;
use crate::index::file_registry::{parse_system_file_rows, SystemFileRows};
use crate::index::index_driver::{run_index, IndexRunOptions};
use crate::index::provenance::{Provenance, K_WALKER_VERSION};
use crate::index::arg_sanitizer::sanitize;
use crate::update::git::{submodule_exclude_globs, top_level};
use crate::update::git_diff::parse_status_porcelain_z;

fn print_index_usage_error(message: &str) {
    eprintln!("symhow index: {}", message);
    eprintln!("Try 'symhow --help' for usage.");
}

/// Resolves the git repo root/HEAD/dirty-paths for `cdb_path`'s provenance.
/// If `repo_dir_flag` is non-empty, that's used as the repo root directly
/// (still validated via `git -C <dir> rev-parse --show-toplevel`); otherwise
/// the parent directory of `cdb_path` is used as the starting point for git
/// discovery. If git isn't on PATH or the directory isn't inside a repo at
/// all, every field is left empty -- NOT an error (just a warning at -v).
/// Mirrors Cli.cpp's buildProvenance().
fn build_provenance(
    cdb_path: &str,
    repo_dir_flag: &str,
    run_options: &IndexRunOptions,
    verbosity: i32,
) -> Provenance {
    let mut prov = Provenance {
        symhow_version: symhow_version().to_string(),
        walker_version: K_WALKER_VERSION,
        cdb_path: absolute_lexically_normal(cdb_path),
        db_options: run_options.db_options.clone(),
        sanitizer_options: run_options.sanitizer_options.clone(),
        system_file_rows: run_options.system_file_rows,
        limit: run_options.limit,
        indexed_at_utc: utc_timestamp_now(),
        ..Default::default()
    };

    let start_dir = if !repo_dir_flag.is_empty() {
        repo_dir_flag.to_string()
    } else {
        absolute_parent(cdb_path)
    };

    let git = |args: &[&str]| process::run_str(args, None);
    let toplevel = git(&["git", "-C", &start_dir, "rev-parse", "--show-toplevel"]);
    if toplevel.exit_code != 0 {
        if verbosity > 0 {
            eprintln!(
                "symhow index: warning: couldn't resolve a git repo root for provenance (from '{}'): {}",
                start_dir,
                trim_trailing_newline(&toplevel.err)
            );
        }
        return prov;
    }
    prov.repo_root = trim_trailing_newline(&toplevel.out);

    let head = git(&["git", "-C", &prov.repo_root, "rev-parse", "HEAD"]);
    if head.exit_code == 0 {
        prov.repo_commit = trim_trailing_newline(&head.out);
    } else if verbosity > 0 {
        eprintln!(
            "symhow index: warning: couldn't resolve HEAD for provenance: {}",
            trim_trailing_newline(&head.err)
        );
    }

    let status = git(&[
        "git",
        "-C",
        &prov.repo_root,
        "status",
        "--porcelain",
        "-z",
        "--untracked-files=all",
    ]);
    if status.exit_code == 0 {
        // Delegate to update::git_diff::parse_status_porcelain_z (handles
        // renames AND copies, exactly like the C++ does).
        prov.dirty_paths = parse_status_porcelain_z(&status.out, &prov.repo_root);
    } else if verbosity > 0 {
        eprintln!(
            "symhow index: warning: couldn't resolve dirty paths for provenance: {}",
            trim_trailing_newline(&status.err)
        );
    }

    prov
}

struct IndexArgs {
    compile_commands_path: String,
    output_path: String,
    db_options: CompileDbOptions,
    sanitizer_options: SanitizerOptions,
    dry_run: bool,
    force: bool,
    quiet_count: i32,
    json_summary: bool,
    no_vacuum: bool,
    system_file_rows: SystemFileRows,
    verbosity: i32,
    limit: i64,
    parallel: i32,
    commit_every: i32,
    stop_after: i64,
    resource_dir_given: bool,
    cuda_path_given: bool,
    gcc_toolchain_given: bool,
    exclude_submodules: bool,
    repo_dir: String,
    notes_dir_flag: String,
}

/// Parses `symhow index` subcommand arguments (argv[start_idx..]) and either
/// runs the --dry-run pipeline (load compile DB -> remap+filter (done inside
/// load()) -> sanitize -> print argv per TU) or the real libclang-backed
/// indexing pipeline (run_index()). Mirrors Cli.cpp's runIndex().
pub fn run_index_cmd(args: &[String], start_idx: usize) -> i32 {
    let mut parsed = IndexArgs {
        compile_commands_path: String::new(),
        output_path: "./symhow.srctrldb".to_string(),
        db_options: CompileDbOptions::default(),
        sanitizer_options: SanitizerOptions::default(),
        dry_run: false,
        force: false,
        quiet_count: 0,
        json_summary: false,
        no_vacuum: false,
        system_file_rows: SystemFileRows::Direct,
        verbosity: 0,
        limit: -1,
        parallel: 1,
        commit_every: 25,
        stop_after: -1,
        resource_dir_given: false,
        cuda_path_given: false,
        gcc_toolchain_given: false,
        exclude_submodules: false,
        repo_dir: String::new(),
        notes_dir_flag: String::new(),
    };

    // Cli.cpp's requireValue lambda: a missing value prints the usage error
    // and std::exit(2)s.
    macro_rules! require_value {
        ($i:expr, $flag:expr) => {{
            if $i + 1 >= args.len() {
                print_index_usage_error(&format!("option '{}' requires a value", $flag));
                return 2;
            }
            args[$i + 1].clone()
        }};
    }

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();

        match arg {
            "-c" | "--compile-commands" => {
                parsed.compile_commands_path = require_value!(i, arg);
                i += 1;
            }
            "-o" | "--output" => {
                parsed.output_path = require_value!(i, arg);
                i += 1;
            }
            "--remap" => {
                let value = require_value!(i, arg);
                i += 1;
                match parse_remap_arg(&value) {
                    Ok(pair) => parsed.db_options.remaps.push(pair),
                    Err(e) => {
                        print_index_usage_error(&e.0);
                        return 2;
                    }
                }
            }
            "--include" => {
                parsed.db_options.include_globs.push(require_value!(i, arg));
                i += 1;
            }
            "--exclude" => {
                parsed.db_options.exclude_globs.push(require_value!(i, arg));
                i += 1;
            }
            "--exclude-submodules" => {
                parsed.exclude_submodules = true;
            }
            "--drop-arg" => {
                parsed.sanitizer_options.drop_arg_patterns.push(require_value!(i, arg));
                i += 1;
            }
            "--extra-arg" => {
                parsed.sanitizer_options.extra_args.push(require_value!(i, arg));
                i += 1;
            }
            "--resource-dir" => {
                parsed.sanitizer_options.resource_dir = require_value!(i, arg);
                parsed.resource_dir_given = true;
                i += 1;
            }
            "--gcc-toolchain" => {
                parsed.sanitizer_options.gcc_toolchain = require_value!(i, arg);
                parsed.gcc_toolchain_given = true;
                i += 1;
            }
            "--force" => {
                parsed.force = true;
            }
            "--repo" => {
                parsed.repo_dir = require_value!(i, arg);
                i += 1;
            }
            "--notes-dir" => {
                parsed.notes_dir_flag = require_value!(i, arg);
                i += 1;
            }
            "--system-file-rows" => {
                let value = require_value!(i, arg);
                i += 1;
                match parse_system_file_rows(&value) {
                    Ok(mode) => parsed.system_file_rows = mode,
                    Err(e) => {
                        print_index_usage_error(&e.0);
                        return 2;
                    }
                }
            }
            "--no-vacuum" => {
                parsed.no_vacuum = true;
            }
            "--json" => {
                parsed.json_summary = true;
            }
            "-q" | "--quiet" => {
                parsed.quiet_count += 1;
            }
            "--parallel" | "-j" => {
                let value = require_value!(i, arg);
                i += 1;
                match cpp_stoi(&value) {
                    Some(v) => parsed.parallel = v,
                    None => {
                        print_index_usage_error(&format!("invalid --parallel/-j value: {}", value));
                        return 2;
                    }
                }
                if parsed.parallel < 0 {
                    print_index_usage_error("--parallel/-j must be >= 0 (0 = hardware_concurrency())");
                    return 2;
                }
            }
            "--cuda-path" => {
                parsed.sanitizer_options.cuda_path = require_value!(i, arg);
                parsed.cuda_path_given = true;
                i += 1;
            }
            "--cuda-mode" => {
                let value = require_value!(i, arg);
                i += 1;
                match parse_cuda_mode(&value) {
                    Ok(mode) => parsed.sanitizer_options.cuda_mode = mode,
                    Err(e) => {
                        print_index_usage_error(&e.0);
                        return 2;
                    }
                }
            }
            "--std" => {
                parsed.sanitizer_options.std_fallback = require_value!(i, arg);
                i += 1;
            }
            "--c-std" => {
                parsed.sanitizer_options.c_std_fallback = require_value!(i, arg);
                i += 1;
            }
            "--limit" => {
                let value = require_value!(i, arg);
                i += 1;
                match cpp_stol(&value) {
                    Some(v) => parsed.limit = v,
                    None => {
                        print_index_usage_error(&format!("invalid --limit value: {}", value));
                        return 2;
                    }
                }
            }
            "--commit-every" => {
                let value = require_value!(i, arg);
                i += 1;
                match cpp_stoi(&value) {
                    Some(v) => parsed.commit_every = v,
                    None => {
                        print_index_usage_error(&format!("invalid --commit-every value: {}", value));
                        return 2;
                    }
                }
                if parsed.commit_every < 0 {
                    print_index_usage_error(
                        "--commit-every must be >= 0 (0 = single transaction, no checkpoints)",
                    );
                    return 2;
                }
            }
            "--stop-after" => {
                // Hidden testing hook -- deliberately not in print_help().
                let value = require_value!(i, arg);
                i += 1;
                match cpp_stol(&value) {
                    Some(v) => parsed.stop_after = v,
                    None => {
                        print_index_usage_error(&format!("invalid --stop-after value: {}", value));
                        return 2;
                    }
                }
            }
            "--dry-run" => {
                parsed.dry_run = true;
            }
            "-v" | "--verbose" => {
                parsed.verbosity += 1;
            }
            "-h" | "--help" => {
                print_help("symhow");
                return 0;
            }
            _ => {
                print_index_usage_error(&format!("unknown option '{}'", arg));
                return 2;
            }
        }
        // Every arm that consumed a value already advanced i to it; this
        // trailing step moves past it (the C++ for-loop's ++i).
        i += 1;
    }

    if parsed.compile_commands_path.is_empty() {
        print_index_usage_error("missing required option -c/--compile-commands PATH");
        return 2;
    }

    // Resolve env-var fallbacks for flags not given explicitly on the CLI.
    // (The compile-time SYMHOW_DEFAULT_RESOURCE_DIR macro fallback for
    // --resource-dir is resolved inside arg_sanitizer itself.)
    if !parsed.resource_dir_given {
        if let Ok(env) = std::env::var("SYMHOW_RESOURCE_DIR") {
            parsed.sanitizer_options.resource_dir = env;
        }
    }
    if !parsed.cuda_path_given {
        if let Ok(env) = std::env::var("SYMHOW_CUDA_PATH") {
            parsed.sanitizer_options.cuda_path = env;
        }
    }
    if !parsed.gcc_toolchain_given {
        if let Ok(env) = std::env::var("SYMHOW_GCC_TOOLCHAIN") {
            parsed.sanitizer_options.gcc_toolchain = env;
        }
    }

    if parsed.exclude_submodules {
        let start_dir = if !parsed.repo_dir.is_empty() {
            parsed.repo_dir.clone()
        } else {
            absolute_parent(&parsed.compile_commands_path)
        };
        let Some(root) = top_level(&start_dir) else {
            print_index_usage_error(&format!(
                "--exclude-submodules: '{}' is not inside a git repository (pass --repo DIR)",
                start_dir
            ));
            return 2;
        };
        for glob in submodule_exclude_globs(&root) {
            if !parsed.db_options.exclude_globs.contains(&glob) {
                parsed.db_options.exclude_globs.push(glob.clone());
                if parsed.verbosity > 0 {
                    eprintln!("symhow index: --exclude-submodules: excluding {}", glob);
                }
            }
        }
    }

    if parsed.dry_run {
        let mut entries = match load(&parsed.compile_commands_path, &parsed.db_options) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("symhow index: {}", e.0);
                return 1;
            }
        };

        if parsed.limit >= 0 && (parsed.limit as usize) < entries.len() {
            entries.truncate(parsed.limit as usize);
        }

        for entry in &entries {
            let result = sanitize(entry, &parsed.sanitizer_options);

            if result.skipped {
                println!("{}: SKIPPED (--cuda-mode skip)", entry.file);
                continue;
            }

            print!("{}:", entry.file);
            for token in &result.args {
                print!(" {}", token);
            }
            println!();
        }

        return 0;
    }

    let mut run_options = IndexRunOptions {
        compile_commands_path: parsed.compile_commands_path.clone(),
        output_path: parsed.output_path.clone(),
        force: parsed.force,
        limit: parsed.limit,
        quiet: parsed.quiet_count >= 1,
        super_quiet: parsed.quiet_count >= 2,
        json_summary: parsed.json_summary,
        no_vacuum: parsed.no_vacuum,
        verbose: parsed.verbosity > 0,
        parallel: parsed.parallel,
        commit_every: parsed.commit_every,
        stop_after: parsed.stop_after,
        db_options: parsed.db_options.clone(),
        sanitizer_options: parsed.sanitizer_options.clone(),
        system_file_rows: parsed.system_file_rows,
        symhow_version: symhow_version().to_string(),
        ..Default::default()
    };

    // Provenance is always built for `symhow index` -- an unresolvable git
    // repo just leaves the repo fields empty, it's never a hard error.
    let mut provenance = build_provenance(
        &parsed.compile_commands_path,
        &parsed.repo_dir,
        &run_options,
        parsed.verbosity,
    );
    if !parsed.notes_dir_flag.is_empty() {
        provenance.notes_dir = absolute_lexically_normal(&parsed.notes_dir_flag);
    }
    run_options.provenance = Some(provenance);

    run_index(&run_options, None)
}
