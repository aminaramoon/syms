#pragma once

#include <cstdint>
#include <string>
#include <vector>

// Phase 3: computes which `file` nodes (and everything that transitively
// depends on them) must be cleared before `symhow update` re-indexes a set of
// changed TUs, and performs that clearing (plus post-clear garbage
// collection) as a single SQL transaction. See docs/KNOWN_LIMITATIONS.md's
// Phase 3 section for the full algorithm write-up and rationale (in
// particular: why GC must run to completion BEFORE any re-indexing writes
// -- `element.id` is a plain sqlite rowid, so deleted ids get reused).
namespace symhow::db
{
class WritableDb;
}

namespace symhow::update
{

// The set of `file` nodes computeClearSet() decided must be cleared, plus
// bookkeeping about how the set grew (for CLI reporting).
struct ClearPlan
{
    std::vector<int64_t> clearedFileIds;
    std::vector<std::string> clearedPaths;  // file.path spellings, one per clearedFileIds entry

    uint32_t seedFiles = 0;           // changed/deleted files, before closure
    uint32_t includeClosureAdded = 0;  // files added by the reverse-INCLUDE closure (step 1)
    uint32_t headersPruned = 0;        // files added by header-pruning (step 2)
};

// Computes the ClearPlan for `changedDbPaths` (files known to have changed
// content/flags, already spelled as file.path) union `deletedDbPaths`
// (files that used to be in the DB and no longer exist / have no CDB
// entry), starting from `db`'s current content. `tuPaths` is every CDB
// entry's `file` (original spelling) -- used by the header-pruning step to
// never prune a file that's still a TU itself. Read-only (does not modify
// `db`); the transaction happens inside invalidate().
ClearPlan computeClearSet(
    symhow::db::WritableDb& db,
    const std::vector<std::string>& changedDbPaths,
    const std::vector<std::string>& deletedDbPaths,
    const std::vector<std::string>& tuPaths);

// Row counts removed/reset by invalidate()/sweepOrphans(), for CLI
// reporting (the `gc` object of symhow.update-summary/1).
struct GcStats
{
    int64_t files = 0;
    int64_t nodes = 0;
    int64_t edges = 0;
    int64_t locations = 0;
    int64_t localSymbols = 0;
    int64_t errors = 0;
    int64_t symbolsReset = 0;
    // Always 0 since Phase 5: the non-indexed-file sweep was removed
    // (see the NOTE in Invalidator.cpp). Kept as a field so the
    // `symhow update --json` GC object's shape does not change.
    int64_t nonIndexedFiles = 0;
    int64_t orphanElements = 0;
};

// Performs the full clear + GC algorithm described in
// docs/KNOWN_LIMITATIONS.md's Phase 3 section against `plan`, inside a
// single BEGIN...COMMIT transaction (rolls back and rethrows on any
// exception). `db` must have been opened with foreignKeys=true (cascades
// are relied on throughout).
GcStats invalidate(symhow::db::WritableDb& db, const ClearPlan& plan);

// Runs the same dead-node/local-symbol/error/non-indexed-file/orphan-
// element sweep invalidate() does in its steps 7-9, but UNRESTRICTED (over
// every node in the database, not just a candidate set) -- a belt-and-
// braces pass run once after re-indexing completes, to catch anything the
// targeted `invalidate()` candidate set might have missed (e.g. facts made
// dead by the NEW content rather than by clearing). Also runs inside its
// own transaction.
GcStats sweepOrphans(symhow::db::WritableDb& db);

}  // namespace symhow::update
