//! Shared path-normalization helper. Mirrors `symhow::common::paths`.

use std::collections::HashMap;
use std::path::Path;

/// Source-path storage contract. An empty root means an unmarked legacy DB.
#[derive(Clone, Debug, Default)]
pub struct SourcePaths {
    pub root: String,
}

impl SourcePaths {
    pub fn from_conn(conn: &rusqlite::Connection) -> Self {
        let format = crate::db::meta::get_meta(conn, "symhow_path_format").unwrap_or_default();
        let root = if format == "relative-v1" {
            crate::db::meta::get_meta(conn, "symhow_source_root").unwrap_or_default()
        } else {
            String::new()
        };
        Self { root }
    }

    pub fn encode(&self, path: &str) -> String {
        encode_source_path(path, &self.root)
    }

    pub fn decode(&self, path: &str) -> String {
        if self.root.is_empty() || path.is_empty() || Path::new(path).is_absolute() {
            path.to_string()
        } else {
            lexical_normalize(&format!("{}/{}", self.root, path))
        }
    }
}

/// Lexical, component-boundary-aware encoding; external absolute paths stay absolute.
/// Does not inspect the filesystem, so moved/deleted files have stable identities.
pub fn encode_source_path(path: &str, root: &str) -> String {
    if root.is_empty() || path.is_empty() {
        return path.to_string();
    }
    let normalized = lexical_normalize(path);
    let root = lexical_normalize(root);
    if !Path::new(&normalized).is_absolute() {
        return normalized;
    }
    if let Ok(relative) = Path::new(&normalized).strip_prefix(&root) {
        let relative = relative.to_string_lossy();
        return if relative.is_empty() { ".".into() } else { relative.into_owned() };
    }
    path.to_string()
}

/// Normalizes `path` for matching purposes ONLY (never for storage/output).
/// Makes it absolute, then canonicalizes it if it exists on disk (resolving
/// symlinks/`.`/`..`), else falls back to lexical normalization.
pub fn normalize(path: &str) -> String {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };

    if abs.exists() {
        if let Ok(canon) = abs.canonicalize() {
            return canon.to_string_lossy().into_owned();
        }
    }
    lexical_normalize(&abs.to_string_lossy())
}

/// Purely textual `.`/`..`/separator normalization (for paths that don't
/// exist on disk). Mirrors `std::filesystem::path::lexically_normal`.
pub fn lexical_normalize(path: &str) -> String {
    let is_abs = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(last) = stack.last() {
                    if *last != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !is_abs {
                    stack.push("..");
                }
            }
            s => stack.push(s),
        }
    }
    let mut result = String::new();
    if is_abs {
        result.push('/');
    }
    result.push_str(&stack.join("/"));
    if result.is_empty() {
        result.push_str(if is_abs { "/" } else { "." });
    }
    result
}

/// Operation-scoped memoization of `normalize()`.
#[derive(Default)]
pub struct NormalizationCache {
    paths: HashMap<String, String>,
}

impl NormalizationCache {
    pub fn resolve(&mut self, path: &str) -> String {
        if let Some(hit) = self.paths.get(path) {
            return hit.clone();
        }
        let normalized = normalize(path);
        self.paths.insert(path.to_string(), normalized.clone());
        normalized
    }

    pub fn size(&self) -> usize {
        self.paths.len()
    }
}