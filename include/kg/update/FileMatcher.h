#pragma once

#include <string>
#include <unordered_set>
#include <vector>

#include "kg/common/Paths.h"
#include "kg/index/CompileDb.h"
#include "kg/update/GitDiff.h"

// Classifies git-reported ChangedFiles against what's already in the
// database (kg_tu.path / file.path -- see MatchInput::dbFilePaths) and the
// freshly-loaded compile_commands.json (MatchInput::cdb), so Phase 3's
// `kg update` knows, for each changed path, whether it's: an already-
// indexed TU that needs re-indexing (changedInDb), a brand-new TU that
// needs indexing for the first time (newTus), a TU that no longer exists
// and needs its facts cleared (deleted), or something git changed that kg
// doesn't otherwise care about (unmatched -- e.g. a header with no direct
// CDB entry, a README, a build script).
namespace kg::update
{

struct MatchInput
{
    std::vector<ChangedFile> changes;
    // Every TU path already recorded in the target database (typically
    // `kg_tu.path`, verbatim -- see the note on path-spelling below).
    std::unordered_set<std::string> dbFilePaths;
    std::vector<kg::index::CompileEntry> cdb;
};

struct MatchResult
{
    // Changed paths that already have a `kg_tu` row -- need re-indexing.
    // Spelled exactly as they appear in `dbFilePaths` (NOT as git spelled
    // them), so later SQL/`kg_tu` lookups can match by string equality.
    std::vector<std::string> changedInDb;
    // Changed paths with no existing `kg_tu` row but a matching CDB entry
    // -- brand-new TUs. Spelled exactly as the CDB's `file` field (NOT as
    // git spelled them).
    std::vector<std::string> newTus;
    // Paths that used to be in the database (deleted or renamed away) and
    // have no CDB entry anymore -- their facts need clearing. Spelled
    // exactly as they appear in `dbFilePaths`.
    std::vector<std::string> deleted;
    // Changed paths that are neither an existing DB TU nor a CDB entry
    // (headers, non-source files, etc). Spelled as git reported them
    // (there's no DB/CDB spelling to prefer).
    std::vector<std::string> unmatched;
};

// Normalizes a path for matching purposes ONLY (never for storage/output --
// see MatchResult's per-field comments on preserving original spelling):
// makes it absolute, then `weakly_canonical()`s it if it exists on disk
// (resolving symlinks/`.`/`..` so e.g. a symlinked source tree and its
// target agree), else falls back to `lexically_normal()` (purely textual,
// for paths that don't exist -- e.g. a file git reports as freshly deleted,
// or a CDB entry for a file that hasn't been generated yet).
//
// This matters because `file.path` in the database comes from clang's
// `clang_getFileName()` while `kg_tu.path`/a CDB entry's `file` come
// verbatim from the CDB post-remap -- they usually agree, but symlinks or a
// literal `./` prefix can make them differ textually while still being
// "the same file".
std::string normalizePath(const std::string& path);

// Classifies every entry in `input.changes` per MatchResult's field
// comments above. Matching is always done on normalizePath() of both
// sides; the ORIGINAL (un-normalized) spelling from `dbFilePaths`/`cdb` is
// what's returned (see MatchResult's comments for exactly which spelling
// each field uses and why).
// Cache-taking overloads let an entire update share the same resolution
// snapshot. Convenience overloads create an operation-local cache.
MatchResult matchChanged(const MatchInput& input);
MatchResult matchChanged(const MatchInput& input, kg::common::paths::NormalizationCache& normalizePath);

// Returns the subset of `entries` whose normalizePath(file) is in
// `normalizedPaths` (already-normalized paths, e.g. from
// MatchResult::newTus run back through normalizePath() by the caller).
std::vector<kg::index::CompileEntry> selectEntries(
    const std::vector<kg::index::CompileEntry>& entries, const std::unordered_set<std::string>& normalizedPaths);
std::vector<kg::index::CompileEntry> selectEntries(
    const std::vector<kg::index::CompileEntry>& entries, const std::unordered_set<std::string>& normalizedPaths,
    kg::common::paths::NormalizationCache& normalizePath);

}  // namespace kg::update
