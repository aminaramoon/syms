//! compile_commands.json loading, remapping and include/exclude filtering.
//! Mirrors `symhow::index::CompileDb`.
//!
//! Deliberately hand-parsed rather than using libclang's
//! clang_CompilationDatabase_fromDirectory(): that API only accepts a
//! directory (hard-codes the filename compile_commands.json), tokenizes
//! arguments with no hook to intervene before its own path handling, and
//! doesn't expose the CDB's optional `output` field. Hand-parsing lets this
//! module run (and be unit-tested) entirely without libclang.

use std::collections::HashMap;

use crate::common::glob::matches;
use crate::common::shell::split;
use crate::common::{Error, Result};
use crate::index::clangutil::{fnv1a64, hex16};

/// A single (already step-1-remapped and include/exclude-filtered) compile
/// database entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileEntry {
    pub directory: String,
    pub file: String,
    pub output: String, // optional; empty if the CDB entry had none
    pub args: Vec<String>, // tokenized argv, INCLUDING argv[0] (program name)
}

/// Options controlling `load()`: remap pairs (applied in order, step 1 of the
/// sanitizing pipeline) and --include/--exclude glob filters (matched against
/// the post-remap absolute `file` path).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompileDbOptions {
    pub remaps: Vec<(String, String)>,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
}

/// Parses a raw "OLD=NEW" --remap argument into a pair. Errors if `spec`
/// contains no '='.
pub fn parse_remap_arg(spec: &str) -> Result<(String, String)> {
    match spec.find('=') {
        Some(eq) => Ok((spec[..eq].to_string(), spec[eq + 1..].to_string())),
        None => Err(Error::new(format!(
            "invalid --remap value (expected OLD=NEW): {}",
            spec
        ))),
    }
}

/// Applies each of `remaps`, in order, as a literal (non-regex) substring
/// substitution (all occurrences) to `text`. Remaps chain: remap N runs
/// against the output of remap N-1.
pub fn apply_remaps(text: &str, remaps: &[(String, String)]) -> String {
    let mut result = text.to_string();
    for (old_str, new_str) in remaps {
        if old_str.is_empty() {
            continue;
        }
        result = result.replace(old_str, new_str);
    }
    result
}

fn matches_any(globs: &[String], path: &str) -> bool {
    globs.iter().any(|g| matches(g, path))
}

/// Loads and parses the compile_commands.json at `path`.
///
/// Each JSON entry must have `directory` and `file`, and EITHER an
/// `arguments` array (used verbatim, including argv[0]) OR a `command`
/// string (tokenized via `common::shell::split`). An `output` field is
/// captured if present. Entries with neither `arguments` nor `command` are
/// skipped.
///
/// Remap substitution (`options.remaps`) is applied to `directory`, `file`,
/// `output`, and every arg token. --include/--exclude glob filtering is then
/// applied against the post-remap `file` path.
pub fn load(path: &str, options: &CompileDbOptions) -> Result<Vec<CompileEntry>> {
    let bytes = std::fs::read(path)
        .map_err(|_| Error::new(format!("cannot open compile database: {}", path)))?;

    let doc: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::new(format!("failed to parse compile database '{}': {}", path, e)))?;

    let entries_json = match doc {
        serde_json::Value::Array(arr) => arr,
        _ => {
            return Err(Error::new(format!(
                "compile database '{}' is not a JSON array",
                path
            )))
        }
    };

    let mut result = Vec::with_capacity(entries_json.len());

    for entry_json in &entries_json {
        let directory = entry_json
            .get("directory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let file = entry_json
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let output = entry_json
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut args: Vec<String> = Vec::new();
        let mut have_args = false;
        if let Some(arguments) = entry_json.get("arguments") {
            if let Some(arr) = arguments.as_array() {
                for tok in arr {
                    args.push(tok.as_str().unwrap_or("").to_string());
                }
                have_args = true;
            }
        }
        if !have_args {
            if let Some(command) = entry_json.get("command").and_then(|v| v.as_str()) {
                args = split(command);
                have_args = true;
            }
        }
        if !have_args {
            // Malformed entry (no way to know what to compile with): skip it
            // rather than failing the whole load.
            continue;
        }

        // Step 1: REMAP -- apply to directory, file, output, and every arg token.
        let directory = apply_remaps(&directory, &options.remaps);
        let file = apply_remaps(&file, &options.remaps);
        let mut output = output;
        if !output.is_empty() {
            output = apply_remaps(&output, &options.remaps);
        }
        let mut args = args;
        for tok in &mut args {
            *tok = apply_remaps(tok, &options.remaps);
        }

        // Include/exclude filtering, matched against the post-remap file path.
        if !options.include_globs.is_empty() && !matches_any(&options.include_globs, &file) {
            continue;
        }
        if matches_any(&options.exclude_globs, &file) {
            continue;
        }

        result.push(CompileEntry {
            directory,
            file,
            output,
            args,
        });
    }

    Ok(result)
}

/// 64-bit FNV-1a over `entry.directory + '\0' + join(entry.args, '\0')`,
/// hex-encoded -- the symhow_tu.args_hash recipe. Computed on the
/// REMAPPED-but-otherwise-raw CompileEntry (as returned by `load()`, before
/// sanitize() rewrites/drops/injects anything).
pub fn compute_args_hash(entry: &CompileEntry) -> String {
    let mut input = String::new();
    input.push_str(&entry.directory);
    input.push('\0');
    for arg in &entry.args {
        input.push_str(arg);
        input.push('\0');
    }
    hex16(fnv1a64(&input))
}

/// Combined per-path args_hash + entry count, as written to symhow_tu once per
/// distinct `file` path in a CDB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathArgsInfo {
    pub combined_hash: String, // 16-hex-char
    pub entry_count: u32,      // number of CDB entries (after remap/include/exclude) with this path
}

/// For each distinct `entry.file`: compute `compute_args_hash()` for every
/// entry with that path, sort the resulting hex strings lexicographically and
/// de-duplicate them, then FNV-1a-64 over the sorted hashes joined with '\0'
/// separators, hex-encoded the same way `compute_args_hash()` is.
///
/// `entry_count` is the raw (non-deduplicated) number of CDB entries observed
/// for that path.
pub fn path_args_hashes(entries: &[CompileEntry]) -> HashMap<String, PathArgsInfo> {
    let mut hashes_by_path: HashMap<String, Vec<String>> = HashMap::new();
    let mut count_by_path: HashMap<String, u32> = HashMap::new();
    for entry in entries {
        hashes_by_path
            .entry(entry.file.clone())
            .or_default()
            .push(compute_args_hash(entry));
        *count_by_path.entry(entry.file.clone()).or_insert(0) += 1;
    }

    let mut result: HashMap<String, PathArgsInfo> = HashMap::new();
    for (path, mut hashes) in hashes_by_path {
        hashes.sort();
        hashes.dedup();

        let mut input = String::new();
        for h in &hashes {
            input.push_str(h);
            input.push('\0');
        }
        let combined_hash = hex16(fnv1a64(&input));

        let info = PathArgsInfo {
            combined_hash,
            entry_count: count_by_path[&path],
        };
        result.insert(path, info);
    }
    result
}