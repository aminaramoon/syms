#include "symhow/update/Invalidator.h"

#include <sqlite3.h>

#include "NodeKind.h"

#include "symhow/db/WritableDb.h"

// See include/symhow/update/Invalidator.h and docs/KNOWN_LIMITATIONS.md's
// Phase 3 section for the algorithm this file implements verbatim.
namespace symhow::update
{

namespace
{

void createPathTempTable(symhow::db::WritableDb& db, const std::string& tableName)
{
    db.exec("DROP TABLE IF EXISTS " + tableName + ";");
    db.exec("CREATE TEMP TABLE " + tableName + "(path TEXT PRIMARY KEY);");
}

void insertPaths(symhow::db::WritableDb& db, const std::string& tableName, const std::vector<std::string>& paths)
{
    const std::string sql = "INSERT OR IGNORE INTO " + tableName + "(path) VALUES(?1)";
    for (const auto& p : paths)
    {
        db.execBound(sql, [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, p.c_str(), -1, SQLITE_TRANSIENT); });
    }
}

void createIdTempTable(symhow::db::WritableDb& db, const std::string& tableName)
{
    db.exec("DROP TABLE IF EXISTS " + tableName + ";");
    db.exec("CREATE TEMP TABLE " + tableName + "(id INTEGER PRIMARY KEY);");
}

void insertIds(symhow::db::WritableDb& db, const std::string& tableName, const std::vector<int64_t>& ids)
{
    const std::string sql = "INSERT OR IGNORE INTO " + tableName + "(id) VALUES(?1)";
    for (int64_t id : ids)
    {
        db.execBound(sql, [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, id); });
    }
}

// Runs `sql` repeatedly (an INSERT OR IGNORE that grows a temp closure
// table) until it stops adding rows. Returns the total number of rows
// added across all iterations.
int64_t runToFixpoint(symhow::db::WritableDb& db, const std::string& sql)
{
    int64_t total = 0;
    for (;;)
    {
        db.exec(sql);
        const int c = db.changes();
        if (c <= 0)
        {
            break;
        }
        total += c;
    }
    return total;
}

}  // namespace

ClearPlan computeClearSet(
    symhow::db::WritableDb& db,
    const std::vector<std::string>& changedDbPaths,
    const std::vector<std::string>& deletedDbPaths,
    const std::vector<std::string>& tuPaths)
{
    ClearPlan plan;

    createPathTempTable(db, "symhow_paths");
    insertPaths(db, "symhow_paths", changedDbPaths);
    insertPaths(db, "symhow_paths", deletedDbPaths);

    createPathTempTable(db, "symhow_tu_paths");
    insertPaths(db, "symhow_tu_paths", tuPaths);

    createIdTempTable(db, "symhow_clear");

    db.exec("INSERT OR IGNORE INTO symhow_clear SELECT id FROM file WHERE path IN (SELECT path FROM symhow_paths);");
    plan.seedFiles = static_cast<uint32_t>(db.changes() > 0 ? db.changes() : 0);

    // step 1: reverse INCLUDE closure -- loop until changes()==0.
    plan.includeClosureAdded = static_cast<uint32_t>(runToFixpoint(
        db,
        "INSERT OR IGNORE INTO symhow_clear SELECT e.source_node_id FROM edge e JOIN symhow_clear c ON c.id=e.target_node_id "
        "WHERE e.type=256;"));

    // step 2: header pruning -- headers whose every includer is cleared,
    // and which are never a TU themselves -- loop until changes()==0.
    plan.headersPruned = static_cast<uint32_t>(runToFixpoint(
        db,
        "INSERT OR IGNORE INTO symhow_clear SELECT f.id FROM file f "
        "WHERE f.id NOT IN (SELECT id FROM symhow_clear) AND f.path NOT IN (SELECT path FROM symhow_tu_paths) "
        "AND EXISTS (SELECT 1 FROM edge e WHERE e.type=256 AND e.target_node_id=f.id) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=256 AND e.target_node_id=f.id "
        "AND e.source_node_id NOT IN (SELECT id FROM symhow_clear));"));

    db.query("SELECT c.id, f.path FROM symhow_clear c JOIN file f ON f.id = c.id", [&](sqlite3_stmt* stmt) {
        const sqlite3_int64 id = sqlite3_column_int64(stmt, 0);
        const char* path = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
        plan.clearedFileIds.push_back(static_cast<int64_t>(id));
        plan.clearedPaths.push_back(path != nullptr ? path : std::string());
    });

    db.exec("DROP TABLE IF EXISTS symhow_clear;");
    db.exec("DROP TABLE IF EXISTS symhow_tu_paths;");
    db.exec("DROP TABLE IF EXISTS symhow_paths;");

    return plan;
}

namespace
{

// Steps 6.5-9 of the algorithm, ALWAYS restricted to a candidate set
// (`candJoin`, a JOIN clause on `node n`): `symhow_cand` for invalidate()'s
// pre-index pass, the name-keyed `symhow_pending_cand` carried across the
// re-index for sweepOrphans()'s post-index pass. `preIndex` additionally
// enables the rules that are only sound BEFORE the re-index runs.
GcStats deadNodeAndOrphanSweep(symhow::db::WritableDb& db, const std::string& candJoin, bool preIndex)
{
    GcStats stats;

    // `symhow_touched` (written by SrctrldbBackend::finish(), emptied by
    // invalidate() before the re-index): the nodes the most recent indexing
    // pass still produces. Nothing in here is garbage, whatever its
    // occurrence/edge shape looks like -- see the step 7 note below.
    // CREATE IF NOT EXISTS so the joins below work against a database
    // written by an older symhow too (empty table => protects nothing =>
    // previous behaviour).
    db.exec("CREATE TABLE IF NOT EXISTS symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;");

    // step 6.5 (belt-and-braces, both the pre-index and the post-index
    // pass): a non-MEMBER edge whose
    // SOURCE node has no occurrence of its own is garbage, full stop -- in
    // a database a fresh `symhow index` would produce, every non-MEMBER edge's
    // source is either a `file` node (e.g. an INCLUDE/MACRO_USAGE edge) or
    // a symbol that itself has at least one occurrence (its own
    // declaration/definition site). Step 6 above only deletes a CANDIDATE
    // edge once IT has zero occurrences of its OWN; it does not (and, being
    // restricted to `symhow_cand`, sometimes cannot -- see below) catch the
    // case where the edge itself still has an occurrence but that
    // occurrence is stale/orphaned relative to its own source node having
    // lost every occurrence of ITS OWN (e.g. the source node's defining
    // file was cleared, but one of the source node's outgoing edges
    // happens to carry an occurrence recorded somewhere else -- see
    // docs/KNOWN_LIMITATIONS.md's Phase 5 section for how this was found:
    // a TU-local Catch2 TEST_CASE function's own occurrence disappears with
    // its file, but a CALL edge from that same function survived because
    // its occurrence outlives the source's own). Deleting these dangling
    // edges FIRST means the dead-node fixpoint right below this can then
    // correctly recognize the now-edge-less, occurrence-less source node
    // as dead too, instead of the edge and the node propping each other up
    // forever. Deliberately NOT restricted to `candJoin` -- an orphaned
    // edge is orphaned regardless of whether its source node happened to be
    // a clearing candidate. Proven safe against a fresh index: the
    // predicate matches 0 rows on a freshly indexed database (asserted by
    // both test/unit/test_incremental_fixture.cpp and
    // test/update/cucs_update_scenario.cmake).
    db.exec(
        "DELETE FROM element WHERE id IN (SELECT e.id FROM edge e "
        "WHERE e.type<>1 AND e.source_node_id NOT IN (SELECT id FROM file) "
        "AND e.source_node_id NOT IN (SELECT node_id FROM symhow_touched) "
        "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.source_node_id));");
    stats.edges += db.changes();

    // step 7: dead candidate nodes -- loop until changes()==0.
    //
    // ALWAYS restricted to a candidate set (`candJoin`), never run over
    // every node in the database. A fresh `symhow index` legitimately leaves
    // behind nodes that match this predicate exactly -- no occurrence of
    // their own, no non-MEMBER edge, no outgoing MEMBER edge, reachable
    // only as the TARGET of their parent's MEMBER edge (implicit template
    // instantiations such as
    // `cucascade::exec::future::then_try<(lambda at prefetching_cache.cpp:768)>`,
    // which the walker registers via ensureSymbol()'s semantic-parent chain
    // but never gets to anchor with an occurrence of its own). Measured on
    // the real cucs corpus: 6 such nodes in a freshly indexed
    // `full_head.srctrldb`. An unrestricted pass therefore DELETES rows a
    // fresh index keeps, which is exactly the `symhow dbdiff` divergence this
    // was found through. Restricting the pass to nodes that were CLEARING
    // CANDIDATES (invalidate(): `symhow_cand`; the post-index sweepOrphans():
    // the `symhow_pending_cand` names carried across the re-index) keeps the
    // cascading cleanup an entire dead subtree depends on -- every symbol
    // declared in a cleared file is a candidate, so a deleted class's
    // members are candidates too, and deleting them first is what makes
    // the (now edge-less) class itself eligible on the next iteration --
    // while making the rule a provable no-op on any database that was
    // never invalidated (no candidates => nothing to delete).
    //
    // The candidate restriction alone is still not enough: a node CAN be a
    // candidate (the old index had a CALL edge to it from a function in a
    // cleared file) and yet be re-created by the re-index in exactly that
    // occurrence-less, MEMBER-target-only shape a fresh index also
    // produces (measured: `semi_future::via<...>`,
    // `std::ranges::__for_each_fn::operator()<...(lambda at
    // prefetching_cache.cpp:774:19)>`). Hence the additional `symhow_touched`
    // guard: whatever the pass that just ran still produces is alive, full
    // stop.
    const std::string deadNodeSql =
        std::string("DELETE FROM element WHERE id IN (SELECT n.id FROM node n ") + candJoin +
        " WHERE n.id NOT IN (SELECT id FROM file) AND n.id NOT IN (SELECT node_id FROM symhow_touched) "
        "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=n.id) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type<>1 AND (e.source_node_id=n.id OR e.target_node_id=n.id)) "
        "AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=1 AND e.source_node_id=n.id));";
    stats.nodes = runToFixpoint(db, deadNodeSql);

    // step 8: local symbols / errors exist only through occurrences; stale
    // non-indexed file rows.
    db.exec("DELETE FROM element WHERE id IN (SELECT id FROM local_symbol) AND id NOT IN (SELECT element_id FROM occurrence);");
    stats.localSymbols = db.changes();

    db.exec("DELETE FROM element WHERE id IN (SELECT id FROM error) AND id NOT IN (SELECT element_id FROM occurrence);");
    stats.errors = db.changes();

    // NOTE (Phase 5): there is deliberately NO "delete edge-less,
    // occurrence-less non-indexed `file` rows" rule here, in either pass.
    // A fresh `symhow index` legitimately produces such rows -- FileRegistry's
    // --system-file-rows policy registers a file the walker then never
    // describes (measured on the cucs corpus:
    // `.pixi/.../kvikio/file_handle.hpp`, `.pixi/.../cudf/version_config.hpp`,
    // libstdc++'s `<random>`) -- and the re-index cannot be relied on to
    // re-create one, because the TU that contributed it may not be in the
    // re-index set at all. Sweeping them therefore DELETES rows a fresh
    // index keeps, in both directions of the comparison (`symhow dbdiff` caught
    // it both as a missing `file` row and as a missing file `node`). Cleared
    // files are deleted by step 5 via `symhow_clear`, which is the only file
    // deletion `symhow update` needs.

    // step 9: orphan element rows (deleting a node/edge does not cascade
    // UP to element).
    db.exec(
        "DELETE FROM element WHERE id NOT IN (SELECT id FROM node) AND id NOT IN (SELECT id FROM edge) "
        "AND id NOT IN (SELECT id FROM local_symbol) AND id NOT IN (SELECT id FROM error);");
    stats.orphanElements = db.changes();

    // Dangling symhow_symbol_def / symhow_tu cleanup: neither table has a real FK
    // (symhow_symbol_def has no FOREIGN KEY at all; symhow_tu.file_node_id is a
    // plain nullable column), so a row that outlives its node/file survives
    // the deletes above untouched. That's a landmine for the NEXT
    // invalidate() run: `element.id`/`file.id` are plain sqlite rowids, so
    // a future re-indexed row can REUSE a dead id and silently "inherit" a
    // stale symhow_symbol_def/symhow_tu link that was never meant for it (see
    // docs/KNOWN_LIMITATIONS.md). Run this unconditionally at the end of
    // both invalidate()'s candidate-restricted pass and sweepOrphans()'s
    // full pass (both call this shared helper) so the two tables can never
    // drift out of sync with node/file.
    db.exec("DELETE FROM symhow_symbol_def WHERE node_id NOT IN (SELECT id FROM node) OR file_node_id NOT IN (SELECT id FROM file);");
    db.exec("UPDATE symhow_tu SET file_node_id = NULL WHERE file_node_id IS NOT NULL AND file_node_id NOT IN (SELECT id FROM file);");

    // Orphan source_location rows: everything above deletes ELEMENTS
    // (nodes/edges/local_symbols/errors), which cascades DOWN to their own
    // `occurrence` rows, but never cleans up a `source_location` row whose
    // last remaining `occurrence` just disappeared that way (a
    // `source_location` is not itself an `element` and has no FK back to
    // one). Harmless for `symhow dbdiff`/Snapshot (locations are derived by
    // joining FROM occurrence, so an occurrence-less location is already
    // invisible there) but left-over clutter otherwise; sweep it here so
    // both invalidate()'s candidate pass and sweepOrphans()'s full pass
    // stay tidy. (The `file_node_id NOT IN (SELECT id FROM node)` delete
    // above this function -- invalidate()'s own step 6 tail -- only
    // catches locations whose FILE died; this catches the more common case
    // of a location whose file is still alive but whose one occurrence
    // just got deleted.)
    db.exec("DELETE FROM source_location WHERE id NOT IN (SELECT source_location_id FROM occurrence);");
    stats.locations += db.changes();

    return stats;
}

}  // namespace

GcStats invalidate(symhow::db::WritableDb& db, const ClearPlan& plan)
{
    GcStats stats;

    db.begin();
    try
    {
        createIdTempTable(db, "symhow_clear");
        insertIds(db, "symhow_clear", plan.clearedFileIds);

        // Empty the previous pass's symhow_touched: from here until the
        // re-index calls SrctrldbBackend::finish() again, NOTHING is
        // "produced by the current pass", so this pre-index sweep behaves
        // exactly as it would without the table at all.
        db.exec("DROP TABLE IF EXISTS symhow_touched;");
        db.exec("CREATE TABLE symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;");

        createPathTempTable(db, "symhow_paths");
        insertPaths(db, "symhow_paths", plan.clearedPaths);

        // step 3: candidates -- elements with an occurrence in a cleared
        // file, closed over edge endpoints and MEMBER ancestors.
        db.exec("DROP TABLE IF EXISTS symhow_cand;");
        db.exec("CREATE TEMP TABLE symhow_cand(id INTEGER PRIMARY KEY);");
        db.exec(
            "INSERT OR IGNORE INTO symhow_cand SELECT o.element_id FROM occurrence o "
            "JOIN source_location l ON l.id=o.source_location_id WHERE l.file_node_id IN (SELECT id FROM symhow_clear);");
        db.exec(
            "INSERT OR IGNORE INTO symhow_cand SELECT source_node_id FROM edge WHERE id IN (SELECT id FROM symhow_cand) "
            "OR source_node_id IN (SELECT id FROM symhow_clear) OR target_node_id IN (SELECT id FROM symhow_clear);");
        db.exec(
            "INSERT OR IGNORE INTO symhow_cand SELECT target_node_id FROM edge WHERE id IN (SELECT id FROM symhow_cand) "
            "OR source_node_id IN (SELECT id FROM symhow_clear) OR target_node_id IN (SELECT id FROM symhow_clear);");
        runToFixpoint(
            db,
            "INSERT OR IGNORE INTO symhow_cand SELECT e.source_node_id FROM edge e JOIN symhow_cand c ON c.id=e.target_node_id "
            "WHERE e.type=1;");

        // step 3.5: carry the candidate NODES across the re-index by
        // serialized_name. `element.id` is a plain sqlite rowid that the
        // re-index can (and does) hand out again, so the post-index sweep
        // cannot be restricted by id; `node.serialized_name` is UNIQUE and
        // is exactly the identity `symhow dbdiff` compares on. A real (not
        // TEMP) table so it survives the connection the re-index opens;
        // sweepOrphans() consumes and drops it.
        db.exec("DROP TABLE IF EXISTS symhow_pending_cand;");
        db.exec("CREATE TABLE symhow_pending_cand(serialized_name TEXT PRIMARY KEY);");
        db.exec(
            "INSERT OR IGNORE INTO symhow_pending_cand(serialized_name) SELECT n.serialized_name FROM node n "
            "JOIN symhow_cand c ON c.id=n.id;");

        // step 4: definition-kind reset -- symbols that lose ALL their
        // declaring files.
        db.exec("DROP TABLE IF EXISTS symhow_lost;");
        db.exec(
            "CREATE TEMP TABLE symhow_lost AS SELECT DISTINCT node_id FROM symhow_symbol_def WHERE file_node_id IN "
            "(SELECT id FROM symhow_clear);");
        db.exec("DELETE FROM symhow_symbol_def WHERE file_node_id IN (SELECT id FROM symhow_clear);");
        db.exec("DELETE FROM symbol WHERE id IN (SELECT node_id FROM symhow_lost) AND id NOT IN (SELECT node_id FROM symhow_symbol_def);");
        stats.symbolsReset = db.changes();

        // step 4b: reset node.type to UNKNOWN for every node in symhow_lost
        // (i.e. every node that lost >= 1 declaring file to this clear --
        // not just the ones that lost ALL of them and got their `symbol`
        // row deleted above). node.type is written upgrade-only (see
        // SrctrldbBackend::flushPending()'s kUpdateNodeTypeSql): once a
        // node is CLASS, a later re-observation of the SAME still-cleared
        // declaration as STRUCT can never turn it back, which would make a
        // genuine source edit (`class Square` -> `struct Square`) silently
        // NOT take effect on `symhow update`, unlike a fresh `symhow index` of the
        // same edited source. Resetting to UNKNOWN here lets the re-index
        // that's about to run re-derive the kind from scratch via the same
        // upgrade-only ladder, so the post-update kind is once again a pure
        // function of the (now-current) set of observations -- exactly
        // like a fresh index -- rather than a fossil of whatever kind a
        // now-cleared declaration used to have.
        //
        // Caveat (documented in docs/KNOWN_LIMITATIONS.md's Phase 8
        // section): if this node's ONLY remaining declaring file is one
        // that is NOT being reindexed (e.g. the class was deleted outright
        // and the node survives solely as a reference target from an
        // untouched TU), nothing re-derives its kind this run, and it can
        // be left at UNKNOWN where a from-scratch `symhow index` would show
        // whatever kind that untouched TU's reference implies. This is the
        // same TU-provenance limitation class as the residual-collision
        // `m_byName` caveat above, not a new gap.
        db.exec(
            "UPDATE node SET type = " + std::to_string(sourcetrail::nodeKindToInt(sourcetrail::NodeKind::UNKNOWN)) +
            " WHERE id IN (SELECT node_id FROM symhow_lost);");
        db.exec("DELETE FROM symhow_tu WHERE file_node_id IN (SELECT id FROM symhow_clear) OR path IN (SELECT path FROM symhow_paths);");

        // step 5: cleared file nodes -- cascades: file, filecontent, their
        // source_locations -> occurrences, INCLUDE/MACRO_USAGE edges
        // touching them.
        db.exec("DELETE FROM element WHERE id IN (SELECT id FROM symhow_clear);");
        stats.files = db.changes();

        // step 6: candidate non-MEMBER edges with no remaining occurrence.
        db.exec(
            "DELETE FROM element WHERE id IN (SELECT e.id FROM edge e WHERE e.type<>1 AND e.id IN (SELECT id FROM symhow_cand) "
            "AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.id));");
        stats.edges = db.changes();

        db.exec("DELETE FROM source_location WHERE file_node_id NOT IN (SELECT id FROM node);");
        stats.locations = db.changes();

        // steps 7-9: dead candidate nodes, local symbols/errors, stale
        // non-indexed files, orphan element rows -- restricted to symhow_cand.
        const GcStats sweep = deadNodeAndOrphanSweep(db, "JOIN symhow_cand c ON c.id=n.id", /*preIndex=*/true);
        stats.edges += sweep.edges;
        stats.locations += sweep.locations;
        stats.nodes = sweep.nodes;
        stats.localSymbols = sweep.localSymbols;
        stats.errors = sweep.errors;
        stats.nonIndexedFiles = sweep.nonIndexedFiles;
        stats.orphanElements = sweep.orphanElements;

        db.exec("DROP TABLE IF EXISTS symhow_lost;");
        db.exec("DROP TABLE IF EXISTS symhow_cand;");
        db.exec("DROP TABLE IF EXISTS symhow_clear;");
        db.exec("DROP TABLE IF EXISTS symhow_paths;");

        db.commit();
    }
    catch (...)
    {
        db.rollback();
        throw;
    }

    return stats;
}

GcStats sweepOrphans(symhow::db::WritableDb& db)
{
    GcStats stats;
    db.begin();
    try
    {
        // Post-index pass: restricted to the candidates invalidate()
        // recorded (by name -- see step 3.5). A node that was never a
        // clearing candidate is none of this sweep's business.
        db.exec("CREATE TABLE IF NOT EXISTS symhow_pending_cand(serialized_name TEXT PRIMARY KEY);");
        stats = deadNodeAndOrphanSweep(db, "JOIN symhow_pending_cand p ON p.serialized_name=n.serialized_name", /*preIndex=*/false);
        db.exec("DROP TABLE IF EXISTS symhow_pending_cand;");
        db.commit();
    }
    catch (...)
    {
        db.rollback();
        throw;
    }
    return stats;
}

}  // namespace symhow::update
