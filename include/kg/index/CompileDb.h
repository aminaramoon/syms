#pragma once

#include <cstdint>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

// compile_commands.json loading, remapping and include/exclude filtering.
//
// Deliberately hand-parsed rather than using libclang's
// clang_CompilationDatabase_fromDirectory(): that API only accepts a
// directory (hard-codes the filename compile_commands.json), tokenizes
// arguments with no hook to intervene before its own path handling, and
// doesn't expose the CDB's optional `output` field. Hand-parsing lets this
// module run (and be unit-tested) entirely without libclang -- see the
// Phase 2 design notes.
namespace kg::index
{

// A single (already step-1-remapped and include/exclude-filtered) compile
// database entry.
struct CompileEntry
{
    std::string directory;
    std::string file;
    std::string output;             // optional; empty if the CDB entry had none
    std::vector<std::string> args;  // tokenized argv, INCLUDING argv[0] (program name)
};

// Options controlling CompileDb::load(): remap pairs (applied in order,
// step 1 of the sanitizing pipeline) and --include/--exclude glob filters
// (matched against the post-remap absolute `file` path).
struct CompileDbOptions
{
    std::vector<std::pair<std::string, std::string>> remaps;
    std::vector<std::string> includeGlobs;
    std::vector<std::string> excludeGlobs;
};

// Parses a raw "OLD=NEW" --remap argument into a pair. Throws
// std::invalid_argument if `spec` contains no '='.
std::pair<std::string, std::string> parseRemapArg(const std::string& spec);

// Applies each of `remaps`, in order, as a literal (non-regex) substring
// substitution (all occurrences) to `text`. Remaps chain: remap N runs
// against the output of remap N-1.
std::string applyRemaps(const std::string& text, const std::vector<std::pair<std::string, std::string>>& remaps);

// Loads and parses the compile_commands.json at `path`.
//
// Each JSON entry must have `directory` and `file`, and EITHER an
// `arguments` array (used verbatim, including argv[0]) OR a `command`
// string (tokenized via kg::common::shell::split()). An `output` field is
// captured if present. Entries with neither `arguments` nor `command` are
// skipped.
//
// Remap substitution (options.remaps) is applied to `directory`, `file`,
// `output`, and every arg token. --include/--exclude glob filtering is then
// applied against the post-remap `file` path: if includeGlobs is
// non-empty, an entry must match at least one include glob to survive;
// any entry matching an exclude glob is dropped regardless.
//
// Throws std::runtime_error if the file can't be opened or isn't a valid
// JSON array.
std::vector<CompileEntry> load(const std::string& path, const CompileDbOptions& options);

// 64-bit FNV-1a over `entry.directory + '\0' + join(entry.args, '\0')`,
// hex-encoded -- the kg_tu.args_hash recipe. Deliberately computed on the
// REMAPPED-but-otherwise-raw CompileEntry (i.e. as returned by load(),
// before sanitize() rewrites/drops/injects anything) so a CDB argv change
// is detected independent of kg's own sanitizer version. Single source of
// truth shared by IndexDriver (writes kg_tu rows) and UpdateDriver (flag-
// change detection) -- see docs/KNOWN_LIMITATIONS.md.
std::string computeArgsHash(const CompileEntry& entry);

// Combined per-path args_hash + entry count, as written to kg_tu (see
// Provenance.h's kKgTuDdl) once per distinct `file` path in a CDB.
struct PathArgsInfo
{
    std::string combinedHash;  // 16-hex-char, see pathArgsHashes()'s doc comment
    uint32_t entryCount = 0;   // number of CDB entries (after remap/include/exclude) with this path
};

// Real compile_commands.json files can list the SAME source `file` more
// than once (e.g. compiled for several targets with different flags) --
// kg_tu is keyed by path, not by (path, entry), so it needs ONE args_hash
// per path that's still sensitive to every one of that path's entries and,
// critically, independent of the ORDER those entries appear in the CDB
// (which can vary run to run, e.g. after `cmake --build` regenerates
// compile_commands.json, without any entry's actual content changing).
//
// For each distinct `entry.file` in `entries`: compute computeArgsHash()
// for every entry with that path, sort the resulting hex strings
// lexicographically and de-duplicate them (two entries with byte-identical
// args collapse to one), then FNV-1a-64 over the sorted hashes joined with
// '\0' separators, hex-encoded the same way computeArgsHash() is. A path
// with exactly one entry therefore reduces to hashing that entry's own
// single hash string -- NOT byte-identical to computeArgsHash(entry)
// itself (the two are deliberately different recipes -- see
// docs/KNOWN_LIMITATIONS.md), but just as stable: unchanged unless that
// entry's own args/directory change.
//
// `entryCount` is the raw (non-deduplicated) number of CDB entries observed
// for that path -- persisted as kg_tu.entry_count.
//
// Single source of truth shared by IndexDriver (writes kg_tu rows) and
// UpdateDriver (flag-change detection) -- see docs/KNOWN_LIMITATIONS.md.
std::unordered_map<std::string, PathArgsInfo> pathArgsHashes(const std::vector<CompileEntry>& entries);

}  // namespace kg::index
