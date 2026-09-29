#pragma once

#include <optional>
#include <string>
#include <vector>

#include "kg/update/GitDiff.h"

// Thin wrappers over kg::common::process::run() for the handful of `git`
// invocations `kg update` needs. Never goes through a shell (same rationale
// as kg::common::process itself -- see Process.h). Mirrors (and is meant to
// eventually replace) the ad-hoc git-shelling already inlined in
// kg::cli::buildProvenance (src/cli/Cli.cpp) for `kg index`'s provenance
// collection -- Phase 2 doesn't touch that code, but Phase 3+ may want to
// route it through here too.
namespace kg::update
{

// Resolves `rev` (e.g. "HEAD", a branch name, a short or full SHA) to its
// full 40-hex-char commit SHA, via
// `git -C repoRoot rev-parse --verify <rev>^{commit}`. Returns std::nullopt
// if git isn't on PATH, `repoRoot` isn't inside a work tree, or `rev`
// doesn't resolve to a commit.
std::optional<std::string> revParse(const std::string& repoRoot, const std::string& rev);

// Resolves the git work tree root containing `anyDir`, via
// `git -C anyDir rev-parse --show-toplevel`. Returns an absolute,
// lexically-normal path, or std::nullopt if `anyDir` isn't inside a work
// tree (or git isn't on PATH).
std::optional<std::string> topLevel(const std::string& anyDir);

// Runs `git -C repoRoot diff --name-status -z --find-renames ...` and
// returns its raw (NUL-separated) stdout, or std::nullopt on failure.
//
// If `to` is std::nullopt, diffs `baseline` against the WORKING TREE (i.e.
// `git diff --name-status -z --find-renames <baseline>` -- no `--cached`,
// so this picks up both staged and unstaged changes, but NOT untracked
// files; git diff never lists those, see collectChanges() below). If `to`
// is set, diffs `baseline..*to` (two fixed commits, no working-tree
// involvement).
std::optional<std::string> diffNameStatusZ(
    const std::string& repoRoot, const std::string& baseline, const std::optional<std::string>& to);

// Runs `git -C repoRoot status --porcelain -z --untracked-files=all` and
// returns its raw (NUL-separated) stdout, or std::nullopt on failure.
std::optional<std::string> statusPorcelainZ(const std::string& repoRoot);

// Absolute, lexically-normal paths of every submodule declared in
// `<repoRoot>/.gitmodules` (read via `git config --file .gitmodules`, so
// submodules need not be initialized/checked out). Empty if there is no
// .gitmodules or git isn't on PATH.
std::vector<std::string> submodulePaths(const std::string& repoRoot);

// `submodulePaths()` turned into kg exclude globs (`<abs path>/**`), for
// `kg index --exclude-submodules` / `kg update --exclude-submodules`.
std::vector<std::string> submoduleExcludeGlobs(const std::string& repoRoot);

// The combined result of collectChanges(): every changed file (from `git
// diff`) plus, when comparing against the working tree, every dirty path
// (staged, unstaged, AND untracked) from `git status` -- see collectChanges'
// own comment for why both are needed.
struct ChangeSet
{
    std::vector<ChangedFile> changes;
    std::vector<std::string> dirtyPaths;
};

// Computes the full changed-file set for `kg update` between `baseline` and
// either the working tree (`to == std::nullopt`) or a second fixed rev
// (`to.has_value()`).
//
// `git diff --name-status` alone never reports untracked files (they have
// nothing to diff against), so when `to` is std::nullopt this ALSO runs
// `git status --porcelain` and adds every `??` (untracked) entry to
// `changes` as ChangeKind::Added -- deduplicated by path against whatever
// `git diff` already reported (a path can't be both "tracked-and-changed"
// and "untracked", but defensive dedup costs nothing). `dirtyPaths` is
// filled with every `git status` entry's (current) path in that same case;
// left empty when `to` is set (a two-fixed-commits diff has no notion of
// "dirty" -- see Provenance.dirtyPaths, which only makes sense for a
// baseline-vs-working-tree update).
//
// Also refuses (returns std::nullopt, with `*err` set) if the repository
// has unmerged paths (a conflicted merge/rebase/cherry-pick still in
// progress) -- detected via statusHasUnmergedEntries() on `git status
// --porcelain` (when `to` is std::nullopt) and/or an Unmerged entry in the
// parsed `git diff --name-status` output. Indexing a conflicted file's
// on-disk content (raw conflict markers) would poison the database, so
// this is treated the same as any other hard failure: the caller must
// resolve the conflict first and re-run.
//
// Returns std::nullopt (with `*err` set, if `err` is non-null) if
// `repoRoot` isn't a git repo, `baseline`/`*to` don't resolve to commits, or
// git isn't on PATH.
std::optional<ChangeSet> collectChanges(
    const std::string& repoRoot, const std::string& baseline, const std::optional<std::string>& to, std::string* err);

}  // namespace kg::update
