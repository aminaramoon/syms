//! Per-TU cache of file identity/status (system-header-ness, whether it's
//! "indexed" per --include/--exclude, and whether it's a direct #include of an
//! indexed file), plus the --system-file-rows policy that decides which
//! non-indexed files still get a FileFact row. Mirrors
//! `symhow::index::FileRegistry`.
//!
//! The C++ implementation keys its cache by libclang's CXFileUniqueID and
//! resolves path/system/content via clang_getFileName /
//! clang_Location_isInSystemHeader / clang_getFileContents. Those libclang
//! specific resolutions land with the index walker; the pure policy logic
//! (indexed decision, shouldRegister) is preserved here exactly, with the
//! caller supplying the resolved `path` / `system` / `content_hash` values.

use std::collections::HashMap;

use crate::common::glob::matches;
use crate::common::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemFileRows {
    None,
    Direct,
    All,
}

/// Parses a --system-file-rows CLI value ("none", "direct", "all").
pub fn parse_system_file_rows(value: &str) -> Result<SystemFileRows> {
    match value {
        "none" => Ok(SystemFileRows::None),
        "direct" => Ok(SystemFileRows::Direct),
        "all" => Ok(SystemFileRows::All),
        _ => Err(Error::new(format!(
            "invalid --system-file-rows value '{}' (expected none, direct, or all)",
            value
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRegistryOptions {
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub system_file_rows: SystemFileRows,
}

impl Default for FileRegistryOptions {
    fn default() -> Self {
        FileRegistryOptions {
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            system_file_rows: SystemFileRows::Direct,
        }
    }
}

/// The libclang CXFileUniqueID triple, used as the cache key so distinct
/// CXFile handles that alias the same on-disk file still share one Info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub path: String,
    pub system: bool,
    pub indexed: bool,
    pub direct_include_of_indexed: bool,
    pub content_hash: String, // indexed files only; exact libclang buffer
}

pub struct FileRegistry {
    options: FileRegistryOptions,
    files: HashMap<FileKey, Info>,
}

impl FileRegistry {
    pub fn new(options: FileRegistryOptions) -> Self {
        FileRegistry {
            options,
            files: HashMap::new(),
        }
    }

    fn matches_any(globs: &[String], path: &str) -> bool {
        globs.iter().any(|g| matches(g, path))
    }

    /// The pure `indexed` decision (mirrors the C++ `info.indexed` assignment
    /// in mutableInfoFor): a non-system file that matches the include globs
    /// (if any) and no exclude glob.
    pub fn is_indexed(&self, system: bool, path: &str) -> bool {
        !system
            && (self.options.include_globs.is_empty()
                || Self::matches_any(&self.options.include_globs, path))
            && !Self::matches_any(&self.options.exclude_globs, path)
    }

    /// Looks up (creating on first sight) the Info for `key`. `path`, `system`,
    /// and `content_hash` (only meaningful for indexed files) are the
    /// libclang-resolved values, supplied by the caller; they are consumed
    /// only the first time a key is seen.
    pub fn info_for(
        &mut self,
        key: FileKey,
        path: &str,
        system: bool,
        content_hash: Option<String>,
    ) -> &Info {
        let indexed = self.is_indexed(system, path);
        self.files.entry(key).or_insert_with(|| Info {
            path: path.to_string(),
            system,
            indexed,
            direct_include_of_indexed: false,
            content_hash: if indexed {
                content_hash.unwrap_or_default()
            } else {
                String::new()
            },
        })
    }

    /// Records that `file` was seen as the direct #include target of an
    /// indexed includer. Resolves-on-first-sight like the C++
    /// mutableInfoFor: the content hash of an indexed (non-system) file is
    /// consumed at FIRST sight of the key, so it must be supplied here too
    /// (a later `info_for()` call can no longer fill it in -- the entry
    /// already exists).
    pub fn mark_direct_include_of_indexed(
        &mut self,
        key: FileKey,
        path: &str,
        system: bool,
        content_hash: Option<String>,
    ) -> &Info {
        let indexed = self.is_indexed(system, path);
        let info = self.files.entry(key).or_insert_with(|| Info {
            path: path.to_string(),
            system,
            indexed,
            direct_include_of_indexed: false,
            content_hash: if indexed {
                content_hash.unwrap_or_default()
            } else {
                String::new()
            },
        });
        info.direct_include_of_indexed = true;
        info
    }

    /// Whether `file` should get a FileFact row: indexed files always qualify;
    /// non-indexed files qualify per the systemFileRows policy (none: never;
    /// direct: only if mark_direct_include_of_indexed() was called for it;
    /// all: always).
    pub fn should_register(&self, key: FileKey) -> bool {
        match self.files.get(&key) {
            Some(info) => {
                if info.indexed {
                    return true;
                }
                match self.options.system_file_rows {
                    SystemFileRows::None => false,
                    SystemFileRows::Direct => info.direct_include_of_indexed,
                    SystemFileRows::All => true,
                }
            }
            None => false,
        }
    }
}