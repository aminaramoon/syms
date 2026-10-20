#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "symhow/db/Database.h"
#include "symhow/query/QueryEngine.h"

// Phase 3 of docs/NOTES_DESIGN.md: `symhow notes scaffold`. Symbol SELECTION
// (--all/--file/--class/SYMBOL) is split from note GENERATION (scaffoldNotes)
// so the CLI can own the exit-code contract for ambiguous/not-found SYMBOL
// and --class resolution (mirroring `symhow query`'s own contract) while the
// actual markdown-writing logic stays a small, independently testable
// function.
namespace symhow::notes
{

// One indexed symbol, resolved to the file it'll be scaffolded into. Shared
// by `symhow notes symbols` (listSymbols()) and `symhow notes scaffold`'s --all/
// --file/--class/SYMBOL selection (listSymbols()/rowForNode()).
struct SymbolRow
{
    std::string sourcePath;  // relative to symhow_repo_root when resolvable, else the raw definition path
    int definitionLine = 0;
    std::string qualifiedName;
    std::string signature;
    std::string bodyHash;
    std::string commit;
    int64_t nodeId = 0;
    std::string serializedName;
};

// Every indexed symbol whose node.type matches `kindMask` (FUNCTION|METHOD
// when kindMask == 0), optionally narrowed to source paths matching any of
// `fileGlobs` (relative to symhow_repo_root; empty = no filtering), sorted by
// (sourcePath, definitionLine, qualifiedName). This is the exact list `symhow
// notes symbols` prints and `symhow notes scaffold --all`/`--file` scaffolds.
std::vector<SymbolRow> listSymbols(symhow::db::Database& db, int kindMask, const std::vector<std::string>& fileGlobs);

// Builds a SymbolRow for one already-resolved node id (used by `symhow notes
// scaffold --class QNAME` -- via QueryEngine::members() -- and `symhow notes
// scaffold SYMBOL`). Returns a default-constructed (nodeId == 0) SymbolRow
// if the node has no definition location.
SymbolRow rowForNode(symhow::db::Database& db, symhow::query::QueryEngine& engine, int64_t nodeId, const std::string& repoRoot, const std::string& commit);

struct ScaffoldOptions
{
    std::string notesDir;
    bool dryRun = false;
};

// Per-target-file outcome of one scaffoldNotes() call.
struct ScaffoldFileResult
{
    std::string path;  // relative to notesDir, e.g. "src/chain.cpp.md"
    bool fileCreated = false;
    std::size_t appended = 0;         // new placeholder blocks written
    std::size_t skippedExisting = 0;  // rows that already had a matching block
};

struct ScaffoldResult
{
    std::vector<ScaffoldFileResult> files;
    std::size_t filesCreated = 0;
    std::size_t filesTouched = 0;  // filesCreated + files that got >=1 appended block
    std::size_t totalAppended = 0;
    std::size_t totalSkipped = 0;
};

// Scaffolds notes for exactly `rows` (already selected by the caller).
// Idempotent and non-destructive (docs/NOTES_DESIGN.md's "Scaffold" rules):
// for each row's target file (`<notesDir>/<sourcePath>.md`), the existing
// file's bytes are NEVER rewritten -- if it exists, it's parsed only to find
// which rows already have a matching block (via matchBlock()), and new
// placeholder blocks (placeholderBlockMarkdown()) are appended verbatim
// after its unchanged existing bytes, in definition-line order; a file with
// nothing new to append is left completely untouched (not even re-read
// bytes are ever written back), so running scaffoldNotes() twice with the
// same `rows` is a byte-identical no-op the second time. A brand-new file is
// created with a `# <sourcePath>` heading. `options.dryRun` computes and
// returns the same ScaffoldResult without writing anything.
ScaffoldResult scaffoldNotes(symhow::db::Database& db, const std::vector<SymbolRow>& rows, const ScaffoldOptions& options);

}  // namespace symhow::notes
