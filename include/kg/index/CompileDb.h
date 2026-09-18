#pragma once

#include <string>
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

}  // namespace kg::index
