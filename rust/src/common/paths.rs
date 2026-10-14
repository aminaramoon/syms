//! Shared path-normalization helper. Mirrors `symhow::common::paths`.

use std::collections::HashMap;
use std::path::Path;

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