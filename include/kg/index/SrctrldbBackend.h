#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#include "kg/model/IndexBackend.h"

// Forward-declared rather than #included here so headers that only need
// IndexBackend-shaped access to SrctrldbBackend (e.g. IndexDriver.h) don't
// have to pull in the vendored writer's headers transitively.
namespace sourcetrail
{
class SourcetrailDBWriter;
struct NameHierarchy;
}  // namespace sourcetrail

namespace kg::db
{
class WritableDb;
}  // namespace kg::db

// SrctrldbBackend.cpp is the ONLY file in kg allowed to #include both
// kg::model types and sourcetrail:: types -- it's the translation boundary
// between the backend-neutral fact model and the vendored SourcetrailDB
// writer.
namespace kg::index
{

class SrctrldbBackend final : public kg::model::IndexBackend
{
public:
    SrctrldbBackend();
    ~SrctrldbBackend() override;

    void setNoVacuum(bool value) { m_noVacuum = value; }
    void setVerbose(bool value) { m_verbose = value; }

    // begin() opens the output db and its schema, but does NOT start the
    // writer's first transaction -- see startTransaction() below. This gap
    // is deliberate: it's where IndexDriver writes provenance (+
    // kg_index_in_progress) via its own tiny WritableDb transaction, BEFORE
    // any TU data is written, satisfying "provenance up front" (Phase 8).
    bool begin(const std::string& dbPath, std::string* err) override;

    // Starts the writer's transaction that consume() calls write into.
    // Must be called exactly once, after begin() and before the first
    // consume()/checkpoint() call.
    bool startTransaction(std::string* err);

    bool consume(const kg::model::TuFactBatch& batch, std::string* err) override;

    // One kg_tu row to upsert once its PATH's LAST CDB entry (in CDB order)
    // has been consumed since the last checkpoint (or since
    // startTransaction(), for the first one) -- see checkpoint()'s doc
    // comment. `argsHash`/`entryCount` are the PATH's combined values (see
    // kg::index::pathArgsHashes()), computed once up front from the WHOLE
    // CDB entry list -- not this one entry's own computeArgsHash() -- so a
    // path with several CDB entries gets a stable, order-independent hash
    // regardless of which of its entries happens to be the LAST one
    // consumed. `status` is the worst outcome (by the caller, IndexDriver)
    // across every one of the path's entries, worst-wins ("failed" >
    // "partial" > "ok"). Deliberately NOT pushed for every entry of a
    // multi-entry path -- only once its LAST entry is consumed -- so that a
    // run interrupted strictly BETWEEN two entries of the same duplicated
    // path leaves NO row for that path at all: `kg update`'s Phase 8 resume
    // logic keys off "no kg_tu row" to find never-indexed entries, and a
    // row written early (on the FIRST entry) would make it wrongly believe
    // an unconsumed sibling entry was already accounted for. See
    // IndexDriver.cpp's `remainingEntriesByPath` and
    // docs/KNOWN_LIMITATIONS.md's "Phase 9" section.
    struct PendingTuRow
    {
        std::string path;       // CompileEntry::file (post-remap spelling)
        std::string argsHash;   // pathArgsHashes()[path].combinedHash
        std::string status;     // "ok" / "partial" / "failed" -- worst-so-far across this path's entries
        uint32_t entryCount = 1;  // pathArgsHashes()[path].entryCount
    };

    // Crash-safe checkpoint: (a) commits the writer's currently-open
    // transaction (so every fact consume()d since the last checkpoint is
    // durably on disk, not just in the hot journal); (b) via a SEPARATE
    // WritableDb connection to the same file (safe: the writer's own
    // transaction is closed at this point), in one transaction: flushes the
    // upgrade-only node.type UPDATE / definition_kind UPSERT for every
    // node touched since the last checkpoint (same SQL, same never-
    // downgrade `<` guard as the old end-of-run-only finish() flush -- see
    // that method's comment for why this makes checkpointed runs produce a
    // byte-for-byte-identical node.type/definition_kind to an uninterrupted
    // single-transaction run: ties are decided by "whichever real kind was
    // observed FIRST", and the `<` guard makes that true whether "first" is
    // measured within one in-memory merge or across N sequential SQL
    // UPDATEs against progressively-updated stored state), the symbol
    // upsert (kg_symbol_def, INSERT OR IGNORE), the kg_touched delta, and
    // `tuRows` as kg_tu upserts; (c) reopens a new writer transaction for
    // subsequent consume() calls. The buffers this drains are cleared after
    // the flush (they only need to hold the since-last-checkpoint delta);
    // the long-lived dedup caches (m_byKey, m_fileNodeIdByPath, etc.) are
    // untouched -- they must persist for the whole session.
    bool checkpoint(const std::vector<PendingTuRow>& tuRows, std::string* err);

    // Interrupt path: identical to checkpoint()'s (a)+(b) above (final
    // flush of `tuRows` and every still-buffered write), but then CLOSES the
    // writer cleanly (no hot journal left) instead of reopening a
    // transaction, and does NOT create reader indices / run ANALYZE/VACUUM /
    // clear kg_index_in_progress -- those are finish()'s job, deferred to
    // whichever `kg update` eventually completes the run. Safe to call at
    // most once, instead of finish().
    bool finishInterrupted(const std::vector<PendingTuRow>& tuRows, std::string* err);

    // IndexBackend::finish() -- unchanged signature (it's a base-class
    // override). Callers MUST call checkpoint() one last time (covering
    // whatever TUs weren't checkpointed yet -- for commitEvery=0 that's
    // simply ONE checkpoint() call covering the entire run) immediately
    // before finish(): by the time finish() runs, every buffer is already
    // empty and every kg_tu row already written, so finish() itself only
    // needs to close out the writer's (by-then-empty) transaction, create
    // reader indices, run ANALYZE/VACUUM, and clear kg_index_in_progress.
    // This makes a commitEvery=0 run's DB byte-for-byte equivalent to the
    // pre-Phase-8 single-flush finish() (one checkpoint = the old flush,
    // performed here instead of duplicated).
    bool finish(kg::model::IndexSummary& summary, std::string* err) override;

private:
    // Shared by checkpoint()/finishInterrupted()/finish(): commits the
    // writer's transaction, then flushes every pending buffered write (+
    // `tuRows`) via m_sideDb in its own transaction. Does NOT touch the
    // writer's transaction state afterward -- callers decide whether to
    // reopen (checkpoint()) or close (finishInterrupted()/finish()).
    bool commitAndFlush(const std::vector<PendingTuRow>& tuRows, std::string* err);
    void flushPending(kg::db::WritableDb& db, const std::vector<PendingTuRow>& tuRows);

    int ensureFileNode(const kg::model::FileFact& fact);
    int ensureFileNodeByPath(const std::string& path, const std::string& language, bool indexed);
    int ensureSymbolNode(const kg::model::SymbolFact& fact);
    int ensureLocalSymbolNode(const std::string& name);
    int ensureReference(int ctxReal, int targetReal, kg::model::ReferenceKind kind);
    void bufferKindDef(int nodeId, kg::model::SymbolKind kind, kg::model::DefinitionKind definition);
    // Resolves batch.stats.tuFile to a real file node id for range-less
    // error attachment: reuses a batch FileFact's already-resolved id when
    // its normalized path matches (avoiding a duplicate `file` row for a
    // relative/non-canonical CDB spelling of the same file clang itself
    // reported canonically), falling back to ensureFileNodeByPath() with the
    // normalized path.
    int resolveTuFileReal(const kg::model::TuFactBatch& batch, const std::vector<int>& fileReal);

    std::unique_ptr<sourcetrail::SourcetrailDBWriter> m_writer;
    std::string m_dbPath;
    bool m_noVacuum = false;
    bool m_verbose = false;

    // Second, independent connection to the same file, used ONLY while the
    // writer's own transaction is closed (between commitTransaction() and
    // the next beginTransaction()/close()) -- see checkpoint()/finish()'s
    // doc comments. Opened once in begin(), right after the writer creates
    // the schema; lives for the whole session.
    std::unique_ptr<kg::db::WritableDb> m_sideDb;

    // path -> real sqlite node id (shared by FileFact rows and
    // SymbolKind::File "file symbols" used as INCLUDE/MACRO_USAGE edge
    // endpoints -- both ultimately resolve to the SAME node, see
    // SrctrldbBackend.cpp's file-symbol handling).
    std::unordered_map<std::string, int> m_fileNodeIdByPath;

    // Non-indexed file ids seen since the last flush -- applied (UPDATE
    // file SET indexed=0,complete=0; DELETE FROM filecontent) by
    // flushPending() on EVERY checkpoint/finish, not just the final one:
    // a file only reached by a TU in an interrupted run's already-
    // committed chunk must not be left with the vendored writer's default
    // indexed=1/complete=1 just because the run never reached its final
    // finish(). Cleared after each flush (delta only); m_anyNonIndexedFiles
    // (never cleared) is what finish() checks to decide whether VACUUM is
    // worthwhile.
    std::unordered_set<int> m_nonIndexedFileIds;
    bool m_anyNonIndexedFiles = false;

    // kg::common::paths::normalize(path) -> real node id, kept in lockstep
    // with m_fileNodeIdByPath -- lets checkpoint()'s kg_tu upsert resolve a
    // PendingTuRow's path to a file_node_id via an in-memory lookup instead
    // of a `SELECT id, path FROM file` scan on every checkpoint.
    std::unordered_map<std::string, int> m_normalizedFileNodeId;

    // Set when this pass materialized at least one reference to the
    // vendored writer's shared "unsolved symbol" node. That node is created
    // INSIDE the writer (recordReferenceToUnsolvedSymhol()), so it never
    // passes through m_byKey and would otherwise be missing from
    // kg_touched -- see finish().
    bool m_sawUnsolvedReference = false;

    // Whether kg_touched has already been (re)created THIS session -- see
    // flushPending()'s comment: the very first flush of a session DROPs and
    // recreates kg_touched (matching the pre-checkpointing finish()
    // behavior -- kg_touched always describes "everything this `kg index`/
    // `kg update` re-index pass touched"); every later flush in the SAME
    // session only INSERTs the delta (m_touchedSinceFlush), never drops.
    bool m_kgTouchedInitialized = false;

    // Node ids touched (created or looked up) since the last flush --
    // drained into kg_touched by flushPending() and cleared after.
    std::unordered_set<int> m_touchedSinceFlush;

    std::unordered_map<std::string, int> m_byKey;           // SymbolKey -> real node id
    std::unordered_map<std::string, std::string> m_byName;  // serialized_name -> SymbolKey (first owner)

    struct BufferedKindDef
    {
        kg::model::SymbolKind kind = kg::model::SymbolKind::Unknown;
        kg::model::DefinitionKind definition = kg::model::DefinitionKind::None;
    };
    std::unordered_map<int, BufferedKindDef> m_pendingKindDef;

    // (node id, file node id) pairs buffered across every consume() call --
    // populated from SymbolFact::declFiles, resolved to real node ids.
    // Drained into kg_symbol_def by finish(). A std::set-like dedup isn't
    // needed here (the eventual INSERT OR IGNORE handles duplicates), but a
    // std::unordered_set of the pair keeps finish()'s statement count sane
    // on TUs that redeclare the same symbol in the same file many times.
    struct NodeFilePairHash
    {
        std::size_t operator()(const std::pair<int, int>& p) const
        {
            return std::hash<int>{}(p.first) ^ (std::hash<int>{}(p.second) << 1);
        }
    };
    std::unordered_set<std::pair<int, int>, NodeFilePairHash> m_pendingSymbolDef;

    std::unordered_map<std::string, int> m_localSymbolIdByName;

    struct RefTripleKey
    {
        int ctx, target, kind;
        bool operator==(const RefTripleKey& o) const { return ctx == o.ctx && target == o.target && kind == o.kind; }
    };
    struct RefTripleHash
    {
        std::size_t operator()(const RefTripleKey& k) const
        {
            std::size_t h = std::hash<int>{}(k.ctx);
            h ^= std::hash<int>{}(k.target) + 0x9e3779b9 + (h << 6) + (h >> 2);
            h ^= std::hash<int>{}(k.kind) + 0x9e3779b9 + (h << 6) + (h >> 2);
            return h;
        }
    };
    std::unordered_map<RefTripleKey, int, RefTripleHash> m_refByTriple;

    kg::model::IndexSummary m_summary;
};

}  // namespace kg::index
