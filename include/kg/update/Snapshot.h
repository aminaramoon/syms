#pragma once

#include <map>
#include <string>
#include <vector>

// A logical, id-independent snapshot of a Sourcetrail-schema database, used
// to verify that `kg update` produces the SAME logical database as a full
// `kg index` re-run (the equivalence-testing contract Phase 2 is building
// toward -- see the `kg dbdiff` hidden CLI command below).
//
// Every category is a MULTISET of "|"-joined string keys built from STABLE,
// content-derived fields only -- never a sqlite `id` (those are
// nondeterministic across separate index runs: insertion order, autovacuum,
// etc can all reassign them) -- represented as key -> occurrence count so
// two logically-identical-content-but-different-COUNT rows (e.g. the same
// local_symbol name appearing twice, once per TU) are NOT silently
// collapsed into one (a plain std::set would hide that divergence). Two
// databases that describe the same code are expected to produce identical
// snapshots (same keys AND same counts) even if their row ids differ
// completely.
namespace kg::update
{

struct Snapshot
{
    std::map<std::string, int> nodes;         // "serialized_name|type"
    std::map<std::string, int> edges;         // "sourceSerializedName|targetSerializedName|type"
    std::map<std::string, int> locations;     // "file.path|start_line|start_col|end_line|end_col|type|elementKey"
    std::map<std::string, int> symbols;       // "serialized_name|definition_kind"
    std::map<std::string, int> files;         // "path|indexed|complete|line_count"
    std::map<std::string, int> errors;        // "message|fatal|filePathOrDash"
    std::map<std::string, int> localSymbols;  // "name"
    std::map<std::string, int> symbolDefs;    // "serialized_name|file.path" (from kg_symbol_def; empty if table absent)
    std::map<std::string, int> tus;           // "path|args_hash" (from kg_tu; empty if table absent)

    // Phase 4 of docs/NOTES_DESIGN.md: `kg notes` cache rows, deliberately
    // NOT keyed by sqlite `id`/`node.id` (notes are keyed by
    // serialized_name, which is exactly the stable identity docs/
    // NOTES_DESIGN.md's "Keyed by stable identity" principle wants a
    // dbdiff to prove is preserved across two independently-indexed
    // databases). Empty (both maps) if `kg_note`/`kg_note_problem` don't
    // exist yet (`kg notes sync` never ran).
    std::map<std::string, int> notes;         // "serialized_name|notes_path|brief|what|how|filled|stale|note_hash"
    std::map<std::string, int> noteProblems;  // "notes_path|line|kind"
};

// Categories a caller may ask takeSnapshot() to leave OUT of the snapshot
// entirely, so `kg dbdiff` can be pointed at a known, documented,
// narrowly-scoped class of divergence instead of being given a looser
// comparison. Each flag must correspond to a limitation spelled out in
// docs/KNOWN_LIMITATIONS.md.
struct SnapshotOptions
{
    // Drop `locations` rows whose file is a SHARED header -- a file that is
    // the target of INCLUDE edges from two or more distinct includers.
    //
    // This is the one class of `kg update` divergence that is inherent to
    // kg's fact granularity rather than a bug: a fact's only provenance in
    // the Sourcetrail schema is the FILE its source_location lives in, not
    // the TU that produced it. Template instantiation makes those two
    // differ -- walking TU `a.cpp` records TYPE_USAGE occurrences whose
    // cursors land in `shared.hpp` (the instantiated pattern's own text)
    // with a context symbol from somewhere else entirely. If `a.cpp`
    // changes, `kg update` clears and re-walks `a.cpp`, but the occurrences
    // `a.cpp`'s OLD content contributed INSIDE `shared.hpp` are not
    // `a.cpp`'s to clear: `shared.hpp` itself is unchanged and is still
    // included by TUs that were not re-indexed, so clearing it would
    // destroy THEIR facts instead. See docs/KNOWN_LIMITATIONS.md's Phase 5
    // section. Nodes, edges, symbols, files, errors, local symbols,
    // symbol_defs and TU rows are NEVER ignored -- only occurrence
    // locations, and only inside multiply-included headers.
    bool ignoreSharedHeaderLocations = false;
};

// Opens `dbPath` read-only (via kg::db::Database) and builds a Snapshot.
// Throws std::runtime_error on the same conditions kg::db::Database's
// constructor does (can't open, wrong/missing storage_version, ...).
Snapshot takeSnapshot(const std::string& dbPath, const SnapshotOptions& options = {});

struct SnapshotDiff
{
    std::vector<std::string> onlyInA;  // category-prefixed, e.g. "node|::foo|4096"
    std::vector<std::string> onlyInB;
};

// Diffs every category of `a` against `b`, prefixing each surviving key
// with its category name (e.g. "node|", "edge|", "location|", "symbol|",
// "file|", "error|", "local_symbol|", "symbol_def|", "tu|", "note|",
// "note_problem|") so the two directions' outputs stay self-describing once
// flattened into one list. A key present in only one side is reported as
// "category|key" in that side's list. A key present on BOTH sides but with
// a different multiset COUNT is reported in both lists as "category|key
// (x<countHere> vs x<countThere>)" -- e.g. "local_symbol|foo (x2 vs x1)" --
// rather than being silently treated as equal. Empty (both directions) iff
// `a` and `b` are logically equivalent (same keys, same counts) per
// Snapshot's field comments.
//
// `only`, if non-empty, restricts the diff to exactly those category names
// (`kg dbdiff --only CATEGORY`) -- e.g. {"note"} to compare ONLY the notes
// cache, ignoring every other category (used by the cross-worktree proof,
// where node/edge/location rows legitimately differ between two clones at
// different filesystem paths but the notes cache -- keyed by
// serialized_name, not node id or path -- should not).
SnapshotDiff diffSnapshots(const Snapshot& a, const Snapshot& b, const std::vector<std::string>& only = {});

}  // namespace kg::update
