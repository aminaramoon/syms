#pragma once

#include <filesystem>
#include <string>
#include <string_view>
#include <vector>

// Pure parsers for `git diff --name-status -z` and `git status --porcelain
// -z` output. No process calls here -- kg::update::Git (Git.h) is the thin
// wrapper that actually shells out to git and hands its stdout to these
// functions. Kept separate so the (fiddly, NUL-delimited) parsing logic can
// be unit-tested against hand-written byte strings without needing a real
// git checkout.
namespace kg::update
{

enum class ChangeKind
{
    Added,
    Modified,
    Deleted,
    Renamed,
    // A path git reports as unmerged (conflict markers still in the
    // worktree/index) -- see parseNameStatusZ's status mapping below.
    // collectChanges() (Git.h) refuses (returns std::nullopt with an error)
    // rather than ever letting one of these reach the invalidator: an
    // unmerged path's on-disk content is not a real revision of the file,
    // so indexing it would poison the database with conflict-marker text.
    Unmerged,
};

// A single changed path, already resolved to an absolute, lexically-normal
// path (joined against the `repoRoot` passed to the parser). For every kind
// except Renamed, only `newPath` is meaningful (`oldPath` is empty) --
// including Deleted, where `newPath` holds the path that was deleted (see
// the header comment on parseNameStatusZ for why: it keeps `ChangedFile`
// uniform instead of branching field names by kind).
struct ChangedFile
{
    ChangeKind kind = ChangeKind::Modified;
    std::string oldPath;
    std::string newPath;
};

// Parses `git diff --name-status -z ...` output (NUL-separated records).
// Each ordinary record is "STATUS\0path\0" (A/M/D/T/U/X/B); rename/copy
// records are "R<score>\0old\0new\0" / "C<score>\0old\0new\0".
//
// Status -> ChangeKind mapping:
//   A (added)                    -> Added
//   M (modified)                 -> Modified
//   T (type change, e.g. symlink<->file) -> Modified
//   D (deleted)                  -> Deleted
//   R<score> (renamed)           -> Renamed (oldPath + newPath both set)
//   C<score> (copied)            -> Added (newPath only -- the copy's
//                                    source is untouched, so from kg's
//                                    perspective a copy just adds a new TU)
//   U (unmerged)                 -> Unmerged
//   X/B (unknown/broken pairing -- rare, only ever seen from `git diff`
//   itself under pathological conditions) -> Modified
//
// Tolerates a trailing NUL and empty input (returns an empty vector).
// `repoRoot` is used to make every path absolute + lexically_normal (the
// paths git prints are already repo-root-relative when the command was run
// with `-C repoRoot`, per kg::update::Git).
std::vector<ChangedFile> parseNameStatusZ(std::string_view nulSeparated, const std::filesystem::path& repoRoot);

// Parses `git status --porcelain -z --untracked-files=all ...` output.
// Each record is "XY path\0", except rename/copy records (X or Y is 'R' or
// 'C') which are "XY new\0old\0" (two NUL-separated tokens). Untracked
// entries are "?? path\0". Ignored entries ("!!") are skipped entirely.
//
// Returns the (absolute, lexically-normal) path of every entry -- for
// renames, only the new (current) path, mirroring
// kg::cli::buildProvenance's existing dirty-paths handling in
// src/cli/Cli.cpp.
std::vector<std::string> parseStatusPorcelainZ(std::string_view nulSeparated, const std::filesystem::path& repoRoot);

// Scans `git status --porcelain -z` output (as returned by statusPorcelainZ)
// for any entry whose X or Y status code is 'U' (unmerged: "UU", "AU",
// "UA", "DU", "UD" -- the full unmerged-pair vocabulary), i.e. a real merge
// conflict still needs resolving in the worktree/index. This is the
// reliable signal for `collectChanges()` (Git.h) to refuse on -- unlike
// `git diff --name-status <baseline>`, which (per real-world testing) folds
// an unmerged path back into a plain 'M' once a baseline ref is given,
// `git status --porcelain` always reports the raw index state.
bool statusHasUnmergedEntries(std::string_view nulSeparated);

}  // namespace kg::update
