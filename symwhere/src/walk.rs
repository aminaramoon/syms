//! Filesystem walk with include/exclude glob filtering.
//! CONTRACT: see SPEC.md `walk.rs`. Owner: indexer agent.
use crate::types::Lang;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Directories that are never interesting for symbol indexing.
const SKIP_DIRS: [&str; 8] = [
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "build",
    ".symwhere",
    ".flox",
];

/// Selects which files a walk should return.
pub struct Filter {
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub langs: BTreeSet<Lang>,
}

/// Walk `root`, returning files (absolute-ish paths as constructed from
/// `root`) that pass the language and glob filters. Order is deterministic
/// (sorted by file name at each level). Directories that are hidden or in
/// [`SKIP_DIRS`] are pruned entirely.
///
/// Include/exclude patterns are matched against repo-relative paths. Absolute
/// patterns are normalized against `root` first (see
/// [`normalize_patterns`]), so `-i /abs/path/to/repo/include` works.
pub fn walk(root: &Path, filter: &Filter) -> Vec<PathBuf> {
    let (excludes, includes) = normalize_patterns(root, &filter.excludes, &filter.includes);
    let filter = Filter {
        excludes,
        includes,
        langs: filter.langs.clone(),
    };
    let mut out = Vec::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| keep_entry(entry, root));
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(lang) = Lang::detect(entry.path()) else {
            continue;
        };
        if !filter.langs.contains(&lang) {
            continue;
        }
        let rel = rel_to_str(rel);
        if filter.excludes.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        if !filter.includes.is_empty() && !filter.includes.iter().any(|pat| glob_match(pat, &rel)) {
            continue;
        }
        out.push(entry.into_path());
    }
    out
}

/// Normalize include/exclude patterns to be repo-relative.
///
/// Patterns are matched against paths relative to `root`, but users naturally
/// pass absolute directory paths (`-i /home/me/repo/include`). For each
/// pattern:
/// - relative patterns pass through unchanged;
/// - absolute patterns under `root` are rebased (with a trailing `/` or a
///   canonicalization mismatch, e.g. `/tmp` vs `/private/tmp`, both handled);
/// - absolute patterns outside `root` can never match: they are kept as inert
///   no-ops (dropping an unmatchable *include* would dangerously widen the
///   filter to "everything") and a warning is printed instead.
///
/// Returns `(excludes, includes)` with warnings emitted on stderr.
fn normalize_patterns(
    root: &Path,
    excludes: &[String],
    includes: &[String],
) -> (Vec<String>, Vec<String>) {
    // Compare against the canonical root so symlinked paths (`/var` vs
    // `/private/var` on macOS) still rebase correctly.
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let norm = |pats: &[String], kind: &str| -> Vec<String> {
        pats.iter()
            .filter_map(|p| normalize_one(&canon_root, p, kind))
            .collect()
    };
    (norm(excludes, "exclude"), norm(includes, "include"))
}

/// Normalize one pattern. Always returns a pattern; outside-root absolutes
/// are warned about and returned unchanged (they match no relative path).
fn normalize_one(root: &Path, pattern: &str, kind: &str) -> Option<String> {
    let path = Path::new(pattern);
    if !path.is_absolute() {
        return Some(pattern.to_string());
    }
    // Absolute pattern directly under the root (handles a trailing slash).
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel_to_str(rel));
    }
    // Canonicalize in case the caller's path goes through a symlink or an
    // unresolved form (`/tmp/...` vs `/private/tmp/...` on macOS). Only the
    // directory prefix can be canonicalized; a trailing glob tail is kept.
    if let Some((existing, tail)) = split_glob_tail(path) {
        if let Ok(canon) = existing.canonicalize() {
            if let Ok(rel) = canon.strip_prefix(root) {
                let mut rel = rel_to_str(rel);
                if !tail.is_empty() {
                    rel.push('/');
                    rel.push_str(tail);
                }
                return Some(rel);
            }
        }
    }
    eprintln!(
        "symwhere: warning: {kind} pattern `{pattern}` is outside the repo root {} \
         and can never match",
        root.display()
    );
    Some(pattern.to_string())
}

/// Split an absolute pattern into its longest existing directory prefix and
/// the remainder (which may contain glob metacharacters), so the prefix can
/// be canonicalized. Returns `(existing_prefix, tail)` where `tail` is `""`
/// when the whole pattern is an existing path.
fn split_glob_tail(pattern: &Path) -> Option<(PathBuf, &str)> {
    let mut current: Option<&Path> = Some(pattern);
    while let Some(dir) = current {
        if dir.exists() {
            let existing = dir.to_path_buf();
            let full = pattern.to_str()?;
            let prefix = existing.to_str()?;
            let tail = full
                .strip_prefix(prefix)
                .unwrap_or_default()
                .trim_start_matches('/');
            return Some((existing, tail));
        }
        current = dir.parent();
    }
    None
}

/// Predicate for `filter_entry`: prune hidden directories and the standard
/// ignore directories. The root itself is always kept.
fn keep_entry(entry: &DirEntry, root: &Path) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    if !entry.file_type().is_dir() {
        return true;
    }
    if entry.path() == root {
        return true;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false; // non-UTF-8 dir name: skip
    };
    if name.starts_with('.') {
        return false;
    }
    !SKIP_DIRS.contains(&name)
}

/// Convert a relative path to a `/`-separated string (best effort).
fn rel_to_str(rel: &Path) -> String {
    let s = rel.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// Match a glob-ish `pattern` against a repo-relative, `/`-separated path.
///
/// Semantics:
/// - `*` matches any run of characters *within* one path segment.
/// - `**` as a whole segment matches any number of segments (including zero).
/// - `?` matches exactly one character within a segment.
/// - A pattern with no glob metacharacters (`*`, `?`) is treated as an exact
///   or prefix segment match: `src/third_party` matches `src/third_party`
///   itself and everything below it.
/// - A slash-less pattern (with or without globs — `*.cpp`, `third_party`,
///   `build*`) matches any single path segment, so it applies at any depth.
/// - Multi-segment glob patterns are matched against the full path,
///   segment by segment (`src/**/*.rs`, `lib/*.rs`, ...).
pub fn glob_match(pattern: &str, rel_path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || rel_path.is_empty() {
        return false;
    }

    if !pattern.contains('*') && !pattern.contains('?') {
        // Literal: exact match or prefix segment match.
        if rel_path == pattern {
            return true;
        }
        if rel_path.len() > pattern.len()
            && rel_path.starts_with(pattern)
            && rel_path.as_bytes()[pattern.len()] == b'/'
        {
            return true;
        }
    }

    if !pattern.contains('/') {
        // Slash-less patterns (literal or glob) match ANY path segment.
        return rel_path
            .split('/')
            .any(|segment| seg_match(pattern.as_bytes(), segment.as_bytes()));
    }

    segs_match(&pattern.split('/').collect::<Vec<_>>(), rel_path)
}

/// Segment-by-segment match supporting a whole-segment `**`.
fn segs_match(pattern: &[&str], rel_path: &str) -> bool {
    match pattern.split_first() {
        None => rel_path.is_empty(),
        Some((&"**", rest)) => {
            let segments: Vec<&str> = rel_path.split('/').collect();
            // `**` may consume zero or more segments.
            (0..=segments.len()).any(|skip| segs_match(rest, segments[skip..].join("/").as_str()))
        }
        Some((pat, rest)) => {
            let (seg, tail) = match rel_path.split_once('/') {
                Some((seg, tail)) => (seg, tail),
                None => (rel_path, ""),
            };
            if tail.is_empty() && !rel_path.contains('/') {
                // Path exhausted: the remaining pattern segments must also be
                // exhausted (`a/b` must not match the file `a`).
                rest.is_empty() && seg_match(pat.as_bytes(), rel_path.as_bytes())
            } else {
                seg_match(pat.as_bytes(), seg.as_bytes()) && segs_match(rest, tail)
            }
        }
    }
}

/// Match one glob segment (`*` and `?`, no `/`) against one path segment.
fn seg_match(pat: &[u8], text: &[u8]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => {
            // Collapse consecutive stars; they are equivalent within a segment.
            let mut rest = rest;
            while let Some(&b'*') = rest.first() {
                rest = &rest[1..];
            }
            if rest.is_empty() {
                return true;
            }
            (0..=text.len()).any(|skip| seg_match(rest, &text[skip..]))
        }
        Some((b'?', rest)) => match text.split_first() {
            Some((_, tail)) => seg_match(rest, tail),
            None => false,
        },
        Some((ch, rest)) => match text.split_first() {
            Some((c, tail)) if c == ch => seg_match(rest, tail),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn set(langs: &[Lang]) -> BTreeSet<Lang> {
        langs.iter().copied().collect()
    }

    #[test]
    fn glob_literal_prefix_segment() {
        assert!(glob_match("src/third_party", "src/third_party"));
        assert!(glob_match(
            "src/third_party",
            "src/third_party/abseil/foo.cpp"
        ));
        assert!(!glob_match("src/third_party", "src/third_partyish/foo.cpp"));
        assert!(!glob_match("src/third_party", "lib/third_party/foo.cpp"));
    }

    #[test]
    fn glob_literal_basename_at_depth() {
        assert!(glob_match("Makefile", "Makefile"));
        assert!(glob_match("third_party", "vendor/third_party/x.cpp"));
        assert!(!glob_match("third_party", "third_partyish/x.cpp"));
    }

    #[test]
    fn glob_star_within_segment() {
        assert!(glob_match("*.cpp", "main.cpp"));
        assert!(glob_match("*.cpp", "src/deep/nested/main.cpp")); // basename at any depth
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/sub/main.rs")); // `*` does not cross `/`
        assert!(!glob_match("*.cpp", "main.cpp.bak"));
        assert!(glob_match("build*", "buildout/gen.rs"));
        assert!(glob_match("build*", "tools/buildout/gen.rs"));
    }

    #[test]
    fn glob_double_star() {
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(glob_match("**/*.rs", "a/b/c/main.rs"));
        assert!(glob_match("src/**/*.rs", "src/main.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/main.rs"));
        assert!(!glob_match("src/**/*.rs", "lib/main.rs"));
        assert!(glob_match("**/test/**", "a/test/b.rs"));
        assert!(!glob_match("**/test/**", "a/testing/b.rs"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("?.cpp", "a.cpp"));
        assert!(!glob_match("?.cpp", "ab.cpp"));
        assert!(glob_match("test_??.rs", "tests/test_ab.rs"));
    }

    #[test]
    fn glob_edge_cases() {
        assert!(!glob_match("", "anything"));
        assert!(!glob_match("*.rs", ""));
        // A pattern with more segments than the path must not match.
        assert!(!glob_match("a/b", "a"));
        assert!(!glob_match("src/gen/**", "src"));
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("**", "main.rs"));
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(!glob_match("*", "")); // nothing to match
    }

    #[test]
    fn walk_filters_by_lang_and_globs() {
        let base = std::env::temp_dir().join(format!(
            "symwhere-walk-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(base.join("src/inner")).unwrap();
        fs::create_dir_all(base.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(base.join("target/debug")).unwrap();
        fs::create_dir_all(base.join(".hidden")).unwrap();
        fs::create_dir_all(base.join("vendor")).unwrap();

        fs::write(base.join("main.rs"), "fn main() {}").unwrap();
        fs::write(base.join("src/lib.rs"), "pub struct S;").unwrap();
        fs::write(base.join("src/inner/mod.rs"), "pub mod m;").unwrap();
        fs::write(base.join("src/inner/detail.cpp"), "int f() { return 0; }").unwrap();
        fs::write(base.join("node_modules/pkg/x.rs"), "fn junk() {}").unwrap();
        fs::write(base.join("target/debug/z.rs"), "fn junk() {}").unwrap();
        fs::write(base.join(".hidden/h.rs"), "fn h() {}").unwrap();
        fs::write(base.join("README.md"), "hi").unwrap();
        fs::write(base.join("vendor/v.rs"), "fn v() {}").unwrap();

        // 1. Only rust, no glob filters. `walk` is sorted by file name at
        // each level, so `src/inner/mod.rs` precedes `src/lib.rs`.
        let filter = Filter {
            excludes: vec![],
            includes: vec![],
            langs: set(&[Lang::Rust]),
        };
        let names = rel_names(&base, walk(&base, &filter));
        assert_eq!(
            names,
            vec!["main.rs", "src/inner/mod.rs", "src/lib.rs", "vendor/v.rs"]
        );

        // 2. Exclude a directory prefix.
        let filter = Filter {
            excludes: vec!["vendor".into()],
            includes: vec![],
            langs: set(&[Lang::Rust]),
        };
        let names = rel_names(&base, walk(&base, &filter));
        assert_eq!(names, vec!["main.rs", "src/inner/mod.rs", "src/lib.rs"]);

        // 3. Include filter restricts further; excludes win over includes.
        let filter = Filter {
            excludes: vec!["src/inner".into()],
            includes: vec!["src/**".into()],
            langs: set(&[Lang::Rust, Lang::Cpp]),
        };
        let names = rel_names(&base, walk(&base, &filter));
        assert_eq!(names, vec!["src/lib.rs"]);

        // 4. Absolute include directories under the root are rebased.
        let filter = Filter {
            excludes: vec![],
            includes: vec![
                base.join("src").to_string_lossy().into_owned(),
                format!("{}/", base.join("src").display()), // trailing slash form
            ],
            langs: set(&[Lang::Rust]),
        };
        let names = rel_names(&base, walk(&base, &filter));
        assert_eq!(names, vec!["src/inner/mod.rs", "src/lib.rs"]);

        // 5. Absolute patterns outside the root never match (kept as inert
        //    no-ops, with a warning).
        let filter = Filter {
            excludes: vec!["/some/other/repo".into()],
            includes: vec!["/elsewhere/include".into()],
            langs: set(&[Lang::Rust]),
        };
        assert!(walk(&base, &filter).is_empty());

        // 6. A canonicalization mismatch (/tmp vs /private/tmp on macOS) is
        //    still rebased when the pattern is an existing absolute path.
        let filter = Filter {
            excludes: vec![],
            includes: vec![base
                .join("src")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()],
            langs: set(&[Lang::Rust]),
        };
        let names = rel_names(&base, walk(&base, &filter));
        assert_eq!(names, vec!["src/inner/mod.rs", "src/lib.rs"]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn normalize_rebases_absolute_patterns() {
        let base = fs::canonicalize(".").unwrap();
        // Under-root absolute → relative.
        let (e, i) = normalize_patterns(
            &base,
            &[format!("{}/third_party", base.display())],
            &[format!("{}", base.join("src").display())],
        );
        assert_eq!(e, vec!["third_party"]);
        assert_eq!(i, vec!["src"]);
        // Outside-root absolute → kept as an inert no-op (never matches).
        let (e, i) = normalize_patterns(&base, &["/etc".to_string()], &["/usr".to_string()]);
        assert_eq!(e, vec!["/etc"]);
        assert_eq!(i, vec!["/usr"]);
        // Relative patterns pass through untouched.
        let (e, i) = normalize_patterns(&base, &["src/gen/**".to_string()], &["*.cpp".to_string()]);
        assert_eq!(e, vec!["src/gen/**"]);
        assert_eq!(i, vec!["*.cpp"]);
    }

    fn rel_names(base: &Path, paths: Vec<PathBuf>) -> Vec<String> {
        paths
            .iter()
            .map(|p| {
                p.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect()
    }
}
