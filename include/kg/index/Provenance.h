#pragma once

#include <optional>
#include <string>
#include <vector>

#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"
#include "kg/index/FileRegistry.h"

struct sqlite3;

// Persisted "how was this database produced" metadata, written to `meta` by
// IndexDriver::writeProvenanceAndTus() (via kg::db::WritableDb) at the end
// of a full `kg index` run. This is the groundwork Phase 2's `kg update` needs to
// decide whether an existing .srctrldb can be incrementally updated at all
// (matching cdb/options) and, if so, which files changed since indexedAt.
namespace kg::db
{
class WritableDb;
}

namespace kg::index
{

// Bump kWalkerVersion whenever AstWalker's emitted facts change in a way
// that would make a database indexed by an older walker inconsistent with
// one indexed by a newer walker (Phase 1 bumps it from the implicit "1"
// to 2, because of the declFiles/DefinitionKind::Explicit semantic change
// -- see docs/KNOWN_LIMITATIONS.md; Phase 5 bumps it to 3, because the
// walker now refuses to emit any fact whose own range lands in a
// NON-INDEXED file -- see AstWalker::rangeFileIndexed()).
inline constexpr int kProvenanceVersion = 1;
inline constexpr int kWalkerVersion = 3;

// Single source of truth for the kg_tu DDL -- both SrctrldbBackend::finish()
// (which creates the table so it always exists, even for a `kg index` run
// that writes no provenance) and IndexDriver's writeProvenanceAndTus() (the
// only place that ever inserts rows into it) need it, and having it in two
// places drifting independently is exactly the kind of bug incremental
// indexing can't afford.
// `status` (added in Phase 4) is the precise per-TU outcome of the most
// recent index/update run that touched this path:
//   - "ok"      -- produced content and no fatal diagnostic (and, for a CUDA
//                  TU resolved via the auto retry ladder, the winning rung
//                  was the first one tried -- not a degraded fallback rung).
//   - "partial" -- produced content, but at least one fatal diagnostic was
//                  recorded, or (CUDA only) the winning rung was a degraded
//                  fallback (NoWrapper/HostOnly, not Full).
//   - "failed"  -- produced no content at all (a total parse/walk failure).
// `kg::update::UpdateDriver` uses `status != 'ok'` as its retry set instead
// of the old (Phase 3) "no outgoing INCLUDE edge" heuristic -- see
// docs/KNOWN_LIMITATIONS.md's Phase 4 section. When a path's several CDB
// entries disagree on status (see `entry_count` below), the WORST one wins
// ("failed" > "partial" > "ok") -- a path is only ever as good as its
// worst-behaved entry.
//
// `entry_count` (added alongside the duplicate-CDB-entry fix) is how many
// CDB entries (after remap/include/exclude) currently share this path --
// almost always 1, but real CDBs can compile the same source file more than
// once (for several targets/configs with different flags). `args_hash` is
// then `kg::index::pathArgsHashes()`'s COMBINED, order-independent hash
// over every one of that path's entries (see CompileDb.h's doc comment),
// not a single entry's `computeArgsHash()` -- a bare per-entry hash would
// make `args_hash` just "whichever entry happened to be written last",
// which made `kg update` see a false "flag changed" on every run for any
// duplicated path (see docs/KNOWN_LIMITATIONS.md). A database whose
// `kg_tu` table predates this column (no `entry_count` column at all) is
// treated as legacy and `kg update` refuses to run against it, same as the
// `status` column above.
inline constexpr const char* kKgTuDdl =
    "CREATE TABLE IF NOT EXISTS kg_tu("
    "path TEXT PRIMARY KEY, "
    "args_hash TEXT NOT NULL, "
    "file_node_id INTEGER, "
    "status TEXT NOT NULL DEFAULT 'ok', "
    "entry_count INTEGER NOT NULL DEFAULT 1"
    ");";

// Optional optimization state. Old databases remain updateable: missing
// hashes conservatively trigger re-indexing, and checkpoints fill them in.
// FK lifetime matches file facts, so invalidation/deletion removes stale hashes.
inline constexpr const char* kKgFileFingerprintDdl =
    "CREATE TABLE IF NOT EXISTS kg_file_fingerprint("
    "file_node_id INTEGER PRIMARY KEY REFERENCES file(id) ON DELETE CASCADE, "
    "sha256 TEXT NOT NULL"
    ");";

struct Provenance
{
    std::string kgVersion;
    int walkerVersion = kWalkerVersion;

    std::string cdbPath;  // absolute path to the compile_commands.json indexed

    CompileDbOptions dbOptions;
    SanitizerOptions sanitizerOptions;
    SystemFileRows systemFileRows = SystemFileRows::Direct;
    long limit = -1;  // IndexRunOptions::limit (-1 = no limit) -- affects which TUs got indexed

    std::string repoRoot;    // absolute; empty if no git repo found
    std::string repoCommit;  // HEAD sha; empty if no git repo found
    std::vector<std::string> dirtyPaths;  // absolute paths, worktree-dirty at index time

    std::string indexedAtUtc;     // ISO-8601 UTC timestamp of the full index that created this provenance
    std::string lastUpdateAtUtc;  // ISO-8601 UTC timestamp of the most recent `kg update` (empty until Phase 2)

    // Phase 3 of docs/NOTES_DESIGN.md: `kg index --notes-dir DIR`'s absolute
    // path, written to its OWN `meta.kg_notes_dir` key (writeProvenance()),
    // deliberately NOT folded into kg_options_json -- changing it must never
    // count as an "option change" for `kg update`'s stored-options
    // comparison. Empty when --notes-dir wasn't given (the default,
    // `<kg_repo_root>/notes` if it exists, is resolved lazily by every
    // notes-aware verb instead -- see Cli.cpp's resolveNotesDirDefault()).
    std::string notesDir;
};

// Writes every Provenance field into `meta` (see the kg_* key list in
// docs/KNOWN_LIMITATIONS.md / the Phase 1 spec) inside the caller's already-
// open transaction (writeProvenance does not itself begin/commit).
void writeProvenance(kg::db::WritableDb& db, const Provenance& prov);

// Reads Provenance back out of `meta`, given any open sqlite3 handle
// (read-only or read-write). Returns std::nullopt if 'kg_provenance_version'
// isn't present (e.g. a pre-Phase-1 database, or one written by the real
// Sourcetrail indexer).
std::optional<Provenance> readProvenance(sqlite3* handle);

}  // namespace kg::index
