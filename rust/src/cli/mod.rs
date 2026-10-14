//! CLI layer: argument parsing and verb dispatch for the `symhow` binary.
//! Mirrors `symhow::cli` (src/cli/Cli.cpp): every verb (`index`, `query`,
//! `update`, `notes`, hidden `dbdiff`) hand-rolls its own argument parsing --
//! exact flags, exact error messages, exact exit codes -- and results go to
//! stdout while diagnostics go to stderr, exactly as the C++ does.
//!
//! The one-file C++ Cli.cpp is split here into one submodule per verb purely
//! for file-size sanity; the behavior contract is identical.

pub mod dbdiff_cmd;
pub mod index_cmd;
pub mod notes_cmd;
pub mod query_cmd;
pub mod update_cmd;

use crate::index::clangutil::{cx_string_to_string, ensure_libclang_loaded};

/// The version string every JSON envelope's `symhow_version` field and
/// `symhow --version` carry (SYMHOW_VERSION_STRING = CMake PROJECT_VERSION in
/// the C++ build; here the crate's own version).
pub fn symhow_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The SourcetrailDB schema version implemented by the vendored
/// third_party/sourcetraildb writer (version.h's VERSION_STRING). The C++
/// bakes this in via a compile definition; the Rust build compiles the same
/// vendored sources, so the constant is just as static here.
const SYMHOW_SRCTRLDB_VERSION_STRING: &str = "v4.db25.p1";

/// The libclang version, mirroring how the C++ build derives it: FindLibClang
/// parses a bare "X.Y.Z" out of the loaded shared library's filename (or
/// leaves "unknown"). The Rust port loads libclang at runtime, so the version
/// is parsed out of `clang_getClangVersion()`'s "clang version X.Y.Z ..."
/// string instead -- same bare "X.Y.Z" shape, same "unknown" fallback.
fn libclang_version() -> String {
    thread_local! {
        static VERSION: String = detect_libclang_version();
    }
    VERSION.with(|v| v.clone())
}

fn detect_libclang_version() -> String {
    ensure_libclang_loaded();
    let full = unsafe { cx_string_to_string(clang_sys::clang_getClangVersion()) };
    if let Some(pos) = full.find("version") {
        let rest = full[pos + "version".len()..].trim_start();
        let mut token = String::new();
        for c in rest.chars() {
            if c.is_ascii_digit() || c == '.' {
                token.push(c);
            } else {
                break;
            }
        }
        let token = token.trim_matches('.').to_string();
        if !token.is_empty() && token.chars().any(|c| c.is_ascii_digit()) {
            return token;
        }
    }
    "unknown".to_string()
}

/// Prints `symhow --version` output to stdout. Mirrors Cli.cpp's printVersion():
/// "symhow <ver>, libclang <ver>, sqlite <ver>, srctrldb <ver>".
pub fn print_version() {
    println!(
        "symhow {}, libclang {}, sqlite {}, srctrldb {}",
        symhow_version(),
        libclang_version(),
        rusqlite::version(),
        SYMHOW_SRCTRLDB_VERSION_STRING
    );
}

/// Prints `symhow --help` output to stdout. Mirrors Cli.cpp's printHelp()
/// byte-for-byte (including the five `programName` substitutions and the
/// trailing exit-codes list).
pub fn print_help(program_name: &str) {
    let text = r#"Usage: {PN} [OPTIONS]
       {PN} index -c/--compile-commands PATH [OPTIONS]
       {PN} query VERB SYMBOL-or-TEXT-or-PATH [OPTIONS]
       {PN} update -d DB [OPTIONS]
       {PN} notes lint --notes-dir DIR [OPTIONS]

symhow indexes C++/CUDA code via libclang into a Sourcetrail-schema-compatible
SQLite database, and queries it.

Options:
  -h, --help       Show this help message and exit
      --version    Show version information and exit

index subcommand:
  -c, --compile-commands PATH   compile_commands.json to load (required)
  -o, --output PATH             output .srctrldb path (default: ./symhow.srctrldb)
      --force                   overwrite --output if it already exists
      --repo DIR                git repo root for provenance metadata (default: discovered
                                 via `git -C <dirname of -c PATH> rev-parse --show-toplevel`)
      --notes-dir DIR           record meta.symhow_notes_dir (absolute) for `symhow notes`/`symhow update`
                                 (default when omitted: <repo root>/notes, resolved lazily)
      --remap OLD=NEW           literal substring path remap (repeatable)
      --include GLOB            only process files matching GLOB (repeatable)
      --exclude GLOB            never process files matching GLOB (repeatable)
      --exclude-submodules      exclude every git submodule listed in the repo's
                                 .gitmodules (adds <submodule>/** exclude globs)
      --drop-arg PATTERN        drop any compiler arg token matching GLOB PATTERN (repeatable)
      --extra-arg ARG           append ARG to every TU's argv, after all built-in
                                 injections (repeatable, order preserved)
      --resource-dir DIR        clang resource dir to inject (default: $SYMHOW_RESOURCE_DIR
                                 or the build's own bundled resource dir)
      --gcc-toolchain DIR       inject --gcc-toolchain=DIR (default: $SYMHOW_GCC_TOOLCHAIN)
      --cuda-path DIR           CUDA toolkit root for CUDA rung synthesis (default:
                                 $SYMHOW_CUDA_PATH, or derived from nvcc's path in the CDB)
      --cuda-mode MODE          auto|full|nowrapper|host-only|skip (default: auto)
      --system-file-rows MODE   none|direct|all: how many non-indexed files get a
                                 `file` table row (default: direct)
      --std STD                 fallback -std= value if none survives sanitizing,
                                 for C++/CUDA TUs (default: c++17)
      --c-std STD               fallback -std= value if none survives sanitizing,
                                 for C TUs (default: gnu17)
      --limit N                 only process the first N (post-filter) TUs
      --no-vacuum               skip the VACUUM step after indexing
      --json                    also print a JSON summary object to stdout
  -j, --parallel N              extract N TUs concurrently (0 = all CPUs, default: 1 =
                                 sequential). Output is byte-for-byte deterministic
                                 regardless of N -- only wall-clock time changes. Peak
                                 memory is roughly N x (largest TU's AST + batch)
      --commit-every N          checkpoint (commit + flush symhow_tu rows) every N consumed
                                 TUs, so Ctrl-C/SIGTERM loses at most N TUs' work instead
                                 of the whole run (default: 25; 0 = single transaction,
                                 no checkpoints). Resume an interrupted run with `symhow
                                 update -d OUTPUT`.
  -q, --quiet                   suppress the per-TU progress line, but keep the periodic
                                 (every ~60s) running-summary line; repeat (-qq) to
                                 silence that too (the final summary always prints)
      --dry-run                 print each TU's fully-sanitized argv and exit,
                                 instead of actually indexing
  -v, --verbose                 increase verbosity (repeatable)

query subcommand:
  VERB                          one of: callers callees bases derived overrides
                                 overridden-by members neighbors references
                                 definition search file stats context
  -d, --db PATH                 database to query (default: $SYMHOW_DB or ./symhow.srctrldb)
      --depth N                 BFS depth limit, 0 = unlimited (default: 1)
      --max-nodes N             BFS node cap, 0 = unlimited (default: 500)
      --kind K                  filter by node kind, e.g. CLASS (repeatable)
      --edge-kind K             filter by edge kind, e.g. CALL (repeatable)
      --location-kind K         filter `file` occurrences by location kind (repeatable)
      --limit N                 result cap for search/stats --errors (default: 100)
      --all-matches             if the symbol is ambiguous, query every candidate
      --signature SIG           narrow symbol resolution by exact signature() match
      --ensure-indices          create the reader-performance indices if missing
      --contents                (file verb) include file contents
      --occurrences             (file verb) include per-location occurrences
      --lines A:B               (file verb) restrict occurrences to a line range
      --errors N                (stats verb) how many error rows to show (default: 100)
      --no-notes                (context verb) omit the "notes" field entirely
      --notes-full              (context verb, text mode) print the full note block, not just the brief
      --json                    print machine-readable JSON instead of text
                                 (see docs/JSON_SCHEMA.md)
  -v, --verbose                 increase verbosity (repeatable)
  -q, --quiet                   suppress the "index incomplete (interrupted)" notice
                                 (printed when DB has symhow_index_in_progress set; never
                                 printed under --json)

update subcommand (incrementally re-index files changed since the last index/update):
  symhow update -d DB                 everything else is inferred from the provenance stored in DB
  -d, --db PATH                   existing .srctrldb produced by this symhow version (required)
      --repo DIR                  git repo (default: stored at index time)
      --to REV                    treat REV as the new baseline (default: HEAD; worktree must match)
  -c, --compile-commands PATH     (default: stored path)
      --files PATH...             explicit changed files instead of git
      --remap/--include/--exclude/--cuda-mode/--system-file-rows/--extra-arg/--drop-arg/--std/--c-std
                                  override stored options (otherwise inherited)
      --exclude-submodules        also exclude every git submodule in the repo's .gitmodules
      --dry-run                   show what would be cleared/re-indexed, change nothing
      --in-place                  modify DB directly instead of temp-copy + atomic rename
  -j, --parallel N                extract N TUs concurrently for the re-index step (0 = all
                                   CPUs, default: 1); deterministic regardless of N
      --no-vacuum, --json, -q, -v

notes subcommand (function-level knowledge attached to the symbol graph --
see docs/NOTES_DESIGN.md):
  lint     --notes-dir DIR [--strict] [--json]   parse a notes tree & report problems (no DB)
  sync     -d DB [--notes-dir DIR] [--strict] [--json]   rebuild the symhow_note* cache
  show     -d DB SYMBOL [--section SEC] [--json]         print one symbol's note
  status   -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]
  symbols  -d DB [--kind K]... [--file GLOB] [--json]    list indexed symbols
  scaffold -d DB [--notes-dir DIR] (--all|--file GLOB|--class QNAME|SYMBOL)
                 [--kind K]... [--dry-run] [--json]      write placeholder note blocks
  accept   -d DB (SYMBOL|--all|--file GLOB) [--json]     re-anchor symhow-hash/symhow-commit
  fmt      --notes-dir DIR [--check] [--json]            canonical re-render (no DB)

Exit codes: 0 ok, 1 usage error, 2 ambiguous symbol / refused (legacy db / update in progress),
            3 not found / lint problems found, 4 database missing/unreadable/wrong version,
            5 nothing to do (update).
"#;
    print!("{}", text.replace("{PN}", program_name));
}

/// Runs the symhow CLI over `args` (where `args[0]` is argv[0], the program
/// name as invoked). Returns the process exit code. Mirrors Cli.cpp's run().
pub fn run(args: &[String]) -> i32 {
    let program_name = args
        .first()
        .cloned()
        .unwrap_or_else(|| "symhow".to_string());

    if args.len() <= 1 {
        print_help(&program_name);
        return 0;
    }

    let first = args[1].as_str();

    match first {
        "index" => index_cmd::run_index_cmd(args, 2),
        "query" => query_cmd::run_query_cmd(args, 2),
        "update" => update_cmd::run_update_cmd(args, 2),
        "notes" => notes_cmd::run_notes(args, 2),
        "dbdiff" => {
            // Hidden maintenance command -- see run_dbdiff()'s own comment.
            // Deliberately not in print_help()'s usage text.
            dbdiff_cmd::run_dbdiff(args, 2)
        }
        _ => {
            for arg in &args[1..] {
                if arg == "--version" {
                    print_version();
                    return 0;
                }
                if arg == "--help" || arg == "-h" {
                    print_help(&program_name);
                    return 0;
                }
                eprintln!("symhow: unknown option '{}'", arg);
                print_help(&program_name);
                return 1;
            }
            0
        }
    }
}

// --- shared helpers (Cli.cpp's anonymous-namespace utilities) -----------------

/// std::filesystem::absolute(p).lexically_normal(): prepend the cwd when
/// relative, then purely textual '.'/'..' normalization -- never resolves
/// symlinks on disk.
pub(crate) fn absolute_lexically_normal(path: &str) -> String {
    let p = std::path::Path::new(path);
    let joined = if p.is_absolute() {
        p.to_string_lossy().into_owned()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
            Err(_) => path.to_string(),
        }
    };
    crate::common::paths::lexical_normalize(&joined)
}

/// std::filesystem::absolute(p).string(): prepend the cwd when relative, NO
/// '.'/'..' normalization at all (used by the notes verbs' --json envelopes).
pub(crate) fn absolute_raw(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        p.to_string_lossy().into_owned()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
            Err(_) => path.to_string(),
        }
    }
}

/// std::filesystem::absolute(p).parent_path(), for any path (relative paths
/// get the cwd prepended first, mirroring Cli.cpp's buildProvenance()).
pub(crate) fn absolute_parent(path: &str) -> String {
    let abs = absolute_lexically_normal(path);
    match abs.rfind('/') {
        Some(0) => "/".to_string(),
        Some(pos) => abs[..pos].to_string(),
        None => String::new(),
    }
}

/// "%Y-%m-%dT%H:%M:%SZ" UTC timestamp of right now (gmtime_r + strftime in
/// the C++). No chrono dependency: computed from SystemTime via plain
/// civil-date arithmetic (Howard Hinnant's days-from-civil algorithm).
pub(crate) fn utc_timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Inverse of days-from-civil (Howard Hinnant's algorithm): year/month/day
/// for a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Trims trailing '\n'/'\r' bytes (git subcommands print one).
pub(crate) fn trim_trailing_newline(s: &str) -> String {
    s.trim_end_matches(|c| c == '\n' || c == '\r').to_string()
}

/// std::stoi semantics: skip leading whitespace, then parse an optionally
/// signed decimal integer from the LEADING digits (the rest of the string is
/// ignored). Returns None when no digits were found at all (the exception
/// case in C++). Values past i32 range saturate, close enough for flag
/// parsing (std::stoi would throw out_of_range; the CLI treats any exception
/// as an invalid value).
pub(crate) fn cpp_stoi(s: &str) -> Option<i32> {
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut idx = 0;
    let mut negative = false;
    if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
        negative = bytes[idx] == b'-';
        idx += 1;
    }
    let digits_start = idx;
    let mut value: i128 = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        value = value * 10 + (bytes[idx] - b'0') as i128;
        idx += 1;
    }
    if idx == digits_start {
        return None; // no digits: std::stoi throws
    }
    let value = if negative { -value } else { value };
    Some(value.clamp(i32::MIN as i128, i32::MAX as i128) as i32)
}

/// std::stol semantics for i64 flag values (--limit/--stop-after).
pub(crate) fn cpp_stol(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut idx = 0;
    let mut negative = false;
    if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
        negative = bytes[idx] == b'-';
        idx += 1;
    }
    let digits_start = idx;
    let mut value: i128 = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        value = value * 10 + (bytes[idx] - b'0') as i128;
        idx += 1;
    }
    if idx == digits_start {
        return None;
    }
    let value = if negative { -value } else { value };
    Some(value.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// std::atoi semantics: like cpp_stoi but 0 on failure (never an error).
pub(crate) fn cpp_atoi(s: &str) -> i32 {
    cpp_stoi(s).unwrap_or(0)
}

/// Cli.cpp's tryParseInt: std::stoi, but the WHOLE string must be consumed
/// (pos == s.size()); empty input fails.
pub(crate) fn try_parse_int_full(s: &str) -> Option<i32> {
    if s.is_empty() {
        return None;
    }
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut idx = 0;
    let mut negative = false;
    if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
        negative = bytes[idx] == b'-';
        idx += 1;
    }
    let digits_start = idx;
    let mut value: i128 = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        value = value * 10 + (bytes[idx] - b'0') as i128;
        idx += 1;
    }
    if idx == digits_start || idx != t.len() {
        return None;
    }
    let value = if negative { -value } else { value };
    Some(value.clamp(i32::MIN as i128, i32::MAX as i128) as i32)
}

/// std::filesystem::relative(full, base).generic_string(), limited to the
/// "full lives under base" case the CLI's notes verbs actually exercise
/// (recursive walks rooted at base). Falls back to the full path when the
/// prefix doesn't match.
pub(crate) fn relative_to_dir(full: &str, base: &str) -> String {
    let full_norm = crate::common::paths::lexical_normalize(full);
    let mut base_norm = crate::common::paths::lexical_normalize(base);
    while base_norm.ends_with('/') && base_norm.len() > 1 {
        base_norm.pop();
    }
    if base_norm == "/" {
        return full_norm.trim_start_matches('/').to_string();
    }
    let prefix = format!("{}/", base_norm);
    if let Some(rest) = full_norm.strip_prefix(&prefix) {
        return rest.to_string();
    }
    if full_norm == base_norm {
        return String::new();
    }
    full_norm
}

/// Recursively collects every regular `*.md` file under `dir` (full paths),
/// mirroring the C++ recursive_directory_iterator walks; NOT sorted here
/// (every caller sorts, exactly like the C++).
pub(crate) fn collect_md_files(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_string()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path.to_string_lossy().into_owned());
            } else if file_type.is_file()
                && path.extension().map(|e| e == "md").unwrap_or(false)
            {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    out
}
