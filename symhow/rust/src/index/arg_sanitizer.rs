//! Argument sanitizing pipeline (steps 2, 3, 5; step 4 is delegated to
//! `cuda_support::synthesize`). Mirrors `symhow::index::ArgSanitizer`.

use std::sync::OnceLock;

use crate::common::glob::matches;
use crate::index::compile_db::CompileEntry;
use crate::index::cuda_support::{self, CudaMode, CudaSynthesisInput};

// The C++ ArgSanitizer.cpp uses a PRIVATE compile definition
// SYMHOW_DEFAULT_RESOURCE_DIR that defaults to "" when unset. The Rust port
// has no such compile-time injection; the empty-string default is inlined
// below (see `sanitize`).
const SYMHOW_DEFAULT_RESOURCE_DIR: &str = "";

/// Options controlling `sanitize()`, populated from `symhow index` CLI flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizerOptions {
    pub drop_arg_patterns: Vec<String>, // --drop-arg glob patterns (step 3c)
    pub extra_args: Vec<String>,        // --extra-arg tokens, appended last (step 5)
    pub resource_dir: String,           // resolved --resource-dir / $SYMHOW_RESOURCE_DIR, or empty
    pub gcc_toolchain: String,          // --gcc-toolchain value, or empty = omit
    pub std_fallback: String,           // --std value or default; C++/CUDA fallback
    pub c_std_fallback: String,         // --c-std value or default; C-only fallback
    pub cuda_path: String,              // resolved --cuda-path / $SYMHOW_CUDA_PATH, or empty
    pub cuda_mode: CudaMode,            // --cuda-mode
}

impl Default for SanitizerOptions {
    fn default() -> Self {
        SanitizerOptions {
            drop_arg_patterns: Vec::new(),
            extra_args: Vec::new(),
            resource_dir: String::new(),
            gcc_toolchain: String::new(),
            std_fallback: "c++17".to_string(),
            c_std_fallback: "gnu17".to_string(),
            cuda_path: String::new(),
            cuda_mode: CudaMode::Auto,
        }
    }
}

/// Result of sanitizing one CompileEntry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeResult {
    pub basename: String,   // argv[0] basename (recorded in step 2), for diagnostics
    pub is_nvcc: bool,
    pub is_host_cxx: bool,
    pub skipped: bool,      // true if CudaMode::Skip applies to this .cu file
    pub cuda_mode_used: CudaMode, // only meaningful if this entry went through CUDA synthesis
    pub args: Vec<String>,  // fully-sanitized argv, EXCLUDING argv[0]
}

fn basename_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// --- Language detection for the step-5 -std fallback ------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuLang {
    Cxx,
    C,
    Cuda,
    ObjC,
    Unknown,
}

fn lang_from_dash_x(lang: &str) -> TuLang {
    match lang {
        "c" | "c-header" | "cpp-output" => TuLang::C,
        "c++" | "c++-header" | "c++-cpp-output" => TuLang::Cxx,
        "cuda" => TuLang::Cuda,
        "objective-c" | "objective-c++" | "objective-c-header" | "objective-c++-header" => TuLang::ObjC,
        _ => TuLang::Unknown,
    }
}

fn lang_from_extension(file: &str) -> TuLang {
    if file.ends_with(".c") {
        return TuLang::C;
    }
    if file.ends_with(".cu") {
        return TuLang::Cuda;
    }
    if file.ends_with(".cpp")
        || file.ends_with(".cc")
        || file.ends_with(".cxx")
        || file.ends_with(".C")
        || file.ends_with(".c++")
    {
        return TuLang::Cxx;
    }
    if file.ends_with(".m") || file.ends_with(".mm") {
        return TuLang::ObjC;
    }
    TuLang::Unknown
}

fn detect_tu_lang(out: &[String], file: &str, argv0_basename: &str) -> TuLang {
    for k in 0..out.len().saturating_sub(1) {
        if out[k] == "-x" {
            let from_x = lang_from_dash_x(&out[k + 1]);
            if from_x != TuLang::Unknown {
                return from_x;
            }
        }
    }

    let from_ext = lang_from_extension(file);
    if from_ext != TuLang::Unknown {
        return from_ext;
    }

    // Ambiguous/unknown extension: use argv0 as a tie-breaker only.
    let argv0_hints_c = !argv0_basename.contains("++") && !argv0_basename.contains("nvcc");
    if argv0_hints_c {
        TuLang::C
    } else {
        TuLang::Cxx
    }
}

/// nvcc lives at <toolkit>/bin/nvcc, so the toolkit root is two directories up.
fn derive_cuda_path_from_nvcc(nvcc_path: &str) -> String {
    if nvcc_path.is_empty() {
        return String::new();
    }
    match std::path::Path::new(nvcc_path).parent() {
        Some(parent) => match parent.parent() {
            Some(grandparent) => grandparent.to_string_lossy().into_owned(),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// Scans `text` for compute_N / sm_N occurrences and updates `max_arch` with
/// the numerically highest N found, if any.
fn update_max_arch(max_arch: &mut i32, text: &str) {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:compute|sm)_([0-9]+)").unwrap());
    for cap in re.captures_iter(text) {
        if let Some(n) = cap.get(1).and_then(|m| m.as_str().parse::<i32>().ok()) {
            if n > *max_arch {
                *max_arch = n;
            }
        }
    }
}

// --- Step 3b: universal drop list ------------------------------------------
//
// Returns the number of tokens (starting at args[i]) to drop, or 0 if this
// token isn't matched by the universal drop list at all (caller then tries
// step 3c).
fn universal_drop_count(args: &[String], i: usize) -> usize {
    let tok = &args[i];
    let has_next = i + 1 < args.len();

    if tok == "-c" {
        return 1;
    }
    if tok == "-o" {
        return if has_next { 2 } else { 1 };
    }
    if tok.starts_with("-o") && tok.len() > 2 {
        return 1; // -o<x> attached form
    }

    if tok == "-MD" || tok == "-MMD" || tok == "-MP" || tok == "-MG" || tok == "-M" || tok == "-MM" {
        return 1;
    }
    if tok == "-MF" || tok == "-MT" || tok == "-MQ" {
        return if has_next { 2 } else { 1 };
    }

    if tok == "-fmodules-ts" {
        return 1;
    }
    if tok.starts_with("-fmodule-mapper=") {
        return 1;
    }
    if tok.starts_with("-fdeps-format=") || tok.starts_with("-fdeps-file=") || tok.starts_with("-fdeps-target=") {
        return 1;
    }

    if tok == "-Werror" || tok.starts_with("-Werror=") {
        return 1;
    }
    if tok == "-pedantic-errors" {
        return 1;
    }

    if tok.starts_with("-fdiagnostics-color") {
        return 1;
    }

    // Link-only flags.
    if tok.starts_with("-l") || tok.starts_with("-L") {
        return 1;
    }
    if tok.starts_with("-Wl,") {
        return 1;
    }
    if tok == "-shared" || tok == "-rdynamic" || tok == "-pie" {
        return 1;
    }
    if tok.starts_with("-fuse-ld=") {
        return 1;
    }

    // We always inject our own -resource-dir in step 5, so any CDB-supplied
    // one is always dropped.
    if tok == "-resource-dir" || tok == "--resource-dir" {
        return if has_next { 2 } else { 1 };
    }
    if tok.starts_with("-resource-dir") || tok.starts_with("--resource-dir") {
        return 1;
    }

    0
}

// --- Step 3a: nvcc-specific rules -------------------------------------------

const DROP_WITH_FOLLOWING_ARG: [&str; 6] = [
    "-Xcudafe",
    "-Xptxas",
    "-Xfatbin",
    "-Xlinker",
    "-ccbin",
    "--compiler-options",
];

fn try_nvcc_rule(args: &[String], i: &mut usize, out: &mut Vec<String>, max_arch: &mut i32) -> bool {
    let tok = &args[*i];
    let has_next = *i + 1 < args.len();

    if tok == "-forward-unknown-to-host-compiler" {
        *i += 1;
        return true;
    }

    // --generate-code / -gencode, separate-argument form.
    if tok == "-gencode" || tok == "--generate-code" {
        if has_next {
            update_max_arch(max_arch, &args[*i + 1]);
            *i += 2;
        } else {
            *i += 1;
        }
        return true;
    }
    // --generate-code=... / -gencode=... , joined form.
    if tok.starts_with("-gencode=") || tok.starts_with("--generate-code=") {
        update_max_arch(max_arch, tok);
        *i += 1;
        return true;
    }
    // --gpu-architecture=compute_N, joined form only.
    if tok.starts_with("--gpu-architecture=") {
        update_max_arch(max_arch, tok);
        *i += 1;
        return true;
    }

    // -Xcompiler=<flag> (joined) unwraps to bare <flag>.
    if let Some(flag) = tok.strip_prefix("-Xcompiler=") {
        out.push(flag.to_string());
        *i += 1;
        return true;
    }
    // -Xcompiler <flag> (separate, two-token) unwraps to bare <flag>.
    if tok == "-Xcompiler" {
        if has_next {
            out.push(args[*i + 1].clone());
            *i += 2;
        } else {
            *i += 1;
        }
        return true;
    }

    if DROP_WITH_FOLLOWING_ARG.contains(&tok.as_str()) {
        *i += if has_next { 2 } else { 1 };
        return true;
    }

    if tok.starts_with("-rdc=") || tok.starts_with("--relocatable-device-code=") {
        *i += 1;
        return true;
    }
    if tok == "-lineinfo" || tok == "-G" {
        *i += 1;
        return true;
    }
    if tok == "--default-stream" {
        *i += if has_next { 2 } else { 1 };
        return true;
    }
    if tok == "--expt-relaxed-constexpr" || tok == "--expt-extended-lambda" || tok == "--extended-lambda" {
        *i += 1;
        return true;
    }

    // -x cu -> -x cuda (two-token form).
    if tok == "-x" && has_next && args[*i + 1] == "cu" {
        out.push("-x".to_string());
        out.push("cuda".to_string());
        *i += 2;
        return true;
    }

    false
}

struct RewriteResult {
    args: Vec<String>,
    max_arch: i32, // -1 = "none seen"; caller applies the 90 default.
}

/// Step 3: single left-to-right rewrite pass. `nvcc_rules_apply` gates step 3a;
/// `drop_arg_patterns` are the user --drop-arg glob patterns (step 3c).
fn rewrite(args: &[String], nvcc_rules_apply: bool, drop_arg_patterns: &[String]) -> RewriteResult {
    let mut result = RewriteResult {
        args: Vec::new(),
        max_arch: -1,
    };

    let mut i = 0usize;
    while i < args.len() {
        if nvcc_rules_apply && try_nvcc_rule(args, &mut i, &mut result.args, &mut result.max_arch) {
            continue;
        }

        let drop_count = universal_drop_count(args, i);
        if drop_count > 0 {
            i += drop_count;
            continue;
        }

        let mut user_dropped = false;
        for pattern in drop_arg_patterns {
            if matches(pattern, &args[i]) {
                user_dropped = true;
                break;
            }
        }
        if user_dropped {
            i += 1;
            continue;
        }

        result.args.push(args[i].clone());
        i += 1;
    }

    result
}

/// Runs steps 2 (split argv0), 3 (rewrite pass), 4 (CUDA synthesis), and 5
/// (injections) against a single (already step-1-remapped) CompileEntry.
pub fn sanitize(entry: &CompileEntry, options: &SanitizerOptions) -> SanitizeResult {
    let mut result = SanitizeResult {
        basename: String::new(),
        is_nvcc: false,
        is_host_cxx: false,
        skipped: false,
        cuda_mode_used: CudaMode::Auto,
        args: Vec::new(),
    };

    // Step 2: split argv0.
    let mut remaining: Vec<String> = entry.args.clone();
    let mut argv0 = String::new();
    if let Some(first) = remaining.first() {
        argv0 = first.clone();
        remaining.remove(0);
    }
    result.basename = basename_of(&argv0);
    result.is_nvcc = result.basename.contains("nvcc");
    result.is_host_cxx = !result.is_nvcc && result.basename.contains("++");

    let is_cu_file = entry.file.ends_with(".cu");
    let nvcc_rules_apply = result.is_nvcc || is_cu_file;

    // Step 3: rewrite pass.
    let rewritten = rewrite(&remaining, nvcc_rules_apply, &options.drop_arg_patterns);
    let mut out = rewritten.args;

    // Step 4: CUDA synthesis (only for .cu files or an nvcc invocation).
    let do_cuda_synthesis = is_cu_file || result.is_nvcc;
    if do_cuda_synthesis {
        let mut cuda_input = CudaSynthesisInput {
            max_arch: if rewritten.max_arch >= 0 {
                rewritten.max_arch
            } else {
                90
            },
            cuda_path: options.cuda_path.clone(),
        };
        if cuda_input.cuda_path.is_empty() && result.is_nvcc {
            cuda_input.cuda_path = derive_cuda_path_from_nvcc(&argv0);
        }

        let resolved_mode = cuda_support::resolve_auto_mode(options.cuda_mode);
        result.cuda_mode_used = resolved_mode;

        if resolved_mode == CudaMode::Skip {
            result.skipped = true;
            return result; // no argv at all for a skipped file
        }

        let cuda_flags = cuda_support::synthesize(resolved_mode, &cuda_input);
        out.extend(cuda_flags);
    }

    // Step 5: injections.
    out.push(format!("-working-directory={}", entry.directory));

    let resource_dir = if options.resource_dir.is_empty() {
        SYMHOW_DEFAULT_RESOURCE_DIR
    } else {
        options.resource_dir.as_str()
    };
    out.push(format!("-resource-dir={}", resource_dir));

    out.push("-fsyntax-only".to_string());
    out.push("-w".to_string());
    out.push("-ferror-limit=0".to_string());

    let has_std = out.iter().any(|tok| tok.starts_with("-std="));
    if !has_std {
        let tu_lang = detect_tu_lang(&out, &entry.file, &result.basename);
        if tu_lang == TuLang::C {
            out.push(format!("-std={}", options.c_std_fallback));
        } else if tu_lang != TuLang::ObjC {
            // C++ and CUDA both use the C++/CUDA fallback.
            out.push(format!("-std={}", options.std_fallback));
        }
    }

    if !options.gcc_toolchain.is_empty() {
        out.push(format!("--gcc-toolchain={}", options.gcc_toolchain));
    }

    for extra in &options.extra_args {
        out.push(extra.clone());
    }

    result.args = out;
    result
}