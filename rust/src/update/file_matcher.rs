//! Classifies git-reported `ChangedFile`s against the database and the CDB.
//! Mirrors `symhow::update::FileMatcher`.

use std::collections::{HashMap, HashSet};

use crate::common::paths::{normalize, NormalizationCache};
use crate::index::compile_db::CompileEntry;

use super::git_diff::{ChangeKind, ChangedFile};

#[derive(Clone, Debug, Default)]
pub struct MatchInput {
    pub changes: Vec<ChangedFile>,
    /// Every TU path already recorded in the target database.
    pub db_file_paths: HashSet<String>,
    pub cdb: Vec<CompileEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct MatchResult {
    /// Changed paths that already have a `symhow_tu` row -- need re-indexing.
    /// Spelled exactly as they appear in `db_file_paths`.
    pub changed_in_db: Vec<String>,
    /// Changed paths with no existing `symhow_tu` row but a matching CDB
    /// entry. Spelled exactly as the CDB's `file` field.
    pub new_tus: Vec<String>,
    /// Paths that used to be in the database and have no CDB entry anymore.
    /// Spelled exactly as they appear in `db_file_paths`.
    pub deleted: Vec<String>,
    /// Changed paths that are neither (headers, non-source files, etc).
    /// Spelled as git reported them.
    pub unmatched: Vec<String>,
}

/// Normalizes a path for matching purposes ONLY (never for storage/output).
pub fn normalize_path(path: &str) -> String {
    normalize(path)
}

fn build_normalized_index(
    originals: &HashSet<String>,
    cache: &mut NormalizationCache,
) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for original in originals {
        index.insert(cache.resolve(original), original.clone());
    }
    index
}

fn build_normalized_cdb_index(cdb: &[CompileEntry], cache: &mut NormalizationCache) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for entry in cdb {
        index.insert(cache.resolve(&entry.file), entry.file.clone());
    }
    index
}

/// Classifies every entry in `input.changes` (operation-local cache).
pub fn match_changed(input: &MatchInput) -> MatchResult {
    let mut cache = NormalizationCache::default();
    match_changed_with(input, &mut cache)
}

/// Cache-taking variant: lets an entire update share one resolution snapshot.
pub fn match_changed_with(input: &MatchInput, cache: &mut NormalizationCache) -> MatchResult {
    let mut result = MatchResult::default();

    let db_index = build_normalized_index(&input.db_file_paths, cache);
    let cdb_index = build_normalized_cdb_index(&input.cdb, cache);

    for change in &input.changes {
        if change.kind == ChangeKind::Deleted || change.kind == ChangeKind::Renamed {
            let removed_path = if change.kind == ChangeKind::Deleted { &change.new_path } else { &change.old_path };
            let norm = cache.resolve(removed_path);
            if let Some(original) = db_index.get(&norm) {
                result.deleted.push(original.clone());
            }
            // else: nothing to clear -- ignore.
        }

        if change.kind == ChangeKind::Added || change.kind == ChangeKind::Modified || change.kind == ChangeKind::Renamed {
            let norm = cache.resolve(&change.new_path);
            if let Some(original) = db_index.get(&norm) {
                result.changed_in_db.push(original.clone());
                continue;
            }
            if let Some(original) = cdb_index.get(&norm) {
                result.new_tus.push(original.clone());
                continue;
            }
            result.unmatched.push(change.new_path.clone());
        }
    }

    result
}

/// Returns the subset of `entries` whose normalized `file` is in
/// `normalized_paths`.
pub fn select_entries(entries: &[CompileEntry], normalized_paths: &HashSet<String>) -> Vec<CompileEntry> {
    let mut cache = NormalizationCache::default();
    select_entries_with(entries, normalized_paths, &mut cache)
}

/// Cache-taking variant.
pub fn select_entries_with(
    entries: &[CompileEntry],
    normalized_paths: &HashSet<String>,
    cache: &mut NormalizationCache,
) -> Vec<CompileEntry> {
    entries
        .iter()
        .filter(|entry| normalized_paths.contains(&cache.resolve(&entry.file)))
        .cloned()
        .collect()
}