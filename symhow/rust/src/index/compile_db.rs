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

/// A root relocation is a component prefix mapping, NOT a user --remap.
/// Each path in a compiler token is transformed once, using its longest old
/// root; paths already beneath current_root always win over historical roots.
pub fn relocate_root_token(text: &str, current_root: &str, roots: &[(String, String)]) -> String {
    if roots.is_empty() { return text.to_string(); }
    fn delimiter(c: u8) -> bool {
        matches!(c, b'=' | b',' | b':' | b';' | b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'@')
    }
    fn left(text: &str, pos: usize) -> bool {
        if pos == 0 || delimiter(text.as_bytes()[pos - 1]) { return true; }
        ["-I", "-F", "-isystem", "-iquote", "-include", "-imacros", "-idirafter", "-isysroot", "-iprefix", "-iwithprefix", "-iwithprefixbefore"]
            .iter().any(|prefix| text[..pos].ends_with(prefix) && {
                let start = pos - prefix.len();
                start == 0 || delimiter(text.as_bytes()[start - 1])
            })
    }
    fn prefix(text: &str, root: &str) -> bool {
        !root.is_empty() && text.starts_with(root) && (root.ends_with('/') ||
            text.len() == root.len() || text.as_bytes()[root.len()] == b'/' || delimiter(text.as_bytes()[root.len()]))
    }
    let current = current_root.trim_end_matches('/');
    let current = if current.is_empty() && current_root.starts_with('/') { "/" } else { current };
    let mut result = String::new();
    let mut pos = 0;
    while pos < text.len() {
        if text.as_bytes()[pos] == b'/' && left(text, pos) {
            if prefix(&text[pos..], current) {
                result.push_str(current);
                pos += current.len();
                continue;
            }
            if let Some((from, to)) = roots.iter().filter(|(from, _)|
                from.starts_with('/') && prefix(&text[pos..], from))
                .max_by_key(|(from, _)| from.len()) {
                result.push_str(to);
                // A root '/' consumes the separator too.
                if from == "/" && !to.ends_with('/') && pos + 1 < text.len() { result.push('/'); }
                pos += from.len();
                continue;
            }
        }
        let ch = text[pos..].chars().next().unwrap();
        result.push(ch);
        pos += ch.len_utf8();
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
    load_relocated(path, options, "", &[])
}

/// As load(), but apply persistent root relocations AFTER user remaps and
/// BEFORE include/exclude filtering. An empty context preserves legacy behavior.
pub fn load_relocated(path: &str, options: &CompileDbOptions, current_root: &str,
    root_remaps: &[(String, String)]) -> Result<Vec<CompileEntry>> {
    let remap = |value: &str| relocate_root_token(&apply_remaps(value, &options.remaps), current_root, root_remaps);
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
        let directory = remap(&directory);
        let file = remap(&file);
        let mut output = output;
        if !output.is_empty() {
            output = remap(&output);
        }
        let mut args = args;
        for tok in &mut args {
            *tok = remap(tok);
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

#[cfg(test)]
mod root_relocation_tests {
    use super::*;

    #[test]
    fn boundaries_current_root_longest_and_no_cascade() {
        let roots = vec![("/old".into(), "/new".into()),
            ("/old/nested".into(), "/special".into()),
            ("/new".into(), "/wrong".into())];
        assert_eq!(relocate_root_token("/old/nested/a.cpp", "/current", &roots), "/special/a.cpp");
        assert_eq!(relocate_root_token("/old/a.cpp", "/current", &roots), "/new/a.cpp");
        assert_eq!(relocate_root_token("/old/a.cpp", "/old", &roots), "/old/a.cpp");
        for flag in ["-I", "-F", "-isystem", "-iquote", "-include", "-imacros", "-idirafter", "-isysroot", "--sysroot=", "@"] {
            assert_eq!(relocate_root_token(&format!("{flag}/old/inc"), "/current", &roots), format!("{flag}/new/inc"));
        }
        for token in ["/old-other/inc", "/external/old/inc", "relative/old/inc", "-DNAME=prefix/old/inc"] {
            assert_eq!(relocate_root_token(token, "/current", &roots), token);
        }
        assert_eq!(relocate_root_token("-Wl,-rpath,/old/lib:/old-other/lib", "/current", &roots), "-Wl,-rpath,/new/lib:/old-other/lib");
        let roots = vec![("/project".into(), "/project-copy".into())];
        assert_eq!(relocate_root_token("/project-copy/src/a.cpp", "/project-copy", &roots), "/project-copy/src/a.cpp");
        assert_eq!(relocate_root_token("/project-copy/src/a.cpp", "/project-copy", &[("/project-copy/src".into(), "/wrong".into())]), "/project-copy/src/a.cpp");
        assert_eq!(relocate_root_token("/project/src/a.cpp", "/project-copy", &roots), "/project-copy/src/a.cpp");
    }

    #[test]
    fn user_remaps_precede_root_relocation_and_filters_follow() {
        let tmp = std::env::temp_dir().join(format!("symhow_root_relocation_{}.json", std::process::id()));
        std::fs::write(&tmp, serde_json::json!([
            {"directory":"/original", "file":"/original/a.cpp", "output":"/original/a.o", "arguments":["clang++", "-I/original/inc", "/original/a.cpp"]},
            {"directory":"/project-copy", "file":"/project-copy/b.cpp", "command":"clang++ -I/project-copy/inc /project-copy/b.cpp"}
        ]).to_string()).unwrap();
        let options = CompileDbOptions { remaps: vec![("/original".into(), "/project".into())],
            include_globs: vec!["/project-copy/*".into()], ..Default::default() };
        let entries = load_relocated(tmp.to_str().unwrap(), &options, "/project-copy", &[("/project".into(), "/project-copy".into())]).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].directory, "/project-copy");
        assert_eq!(entries[0].file, "/project-copy/a.cpp");
        assert_eq!(entries[0].output, "/project-copy/a.o");
        assert_eq!(entries[0].args[1], "-I/project-copy/inc");
        assert_eq!(entries[1].file, "/project-copy/b.cpp");
        assert_eq!(entries[1].args[1], "-I/project-copy/inc");
        std::fs::remove_file(tmp).unwrap();
    }
}
