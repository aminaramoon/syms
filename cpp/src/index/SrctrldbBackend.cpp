#include "symhow/index/SrctrldbBackend.h"

#include <cstdio>
#include <sstream>

#include <sqlite3.h>

#include "DefinitionKind.h"
#include "NameHierarchy.h"
#include "NodeKind.h"
#include "ReferenceKind.h"
#include "SourceRange.h"
#include "SourcetrailDBWriter.h"
#include "SymbolKind.h"

#include "symhow/common/Paths.h"
#include "symhow/db/Meta.h"
#include "symhow/db/WritableDb.h"
#include "symhow/index/Provenance.h"

namespace symhow::index
{

namespace
{

sourcetrail::NameHierarchy toNameHierarchy(const symhow::model::Name& name)
{
    sourcetrail::NameHierarchy nh;
    nh.nameDelimiter = name.delimiter;
    nh.nameElements.reserve(name.elements.size());
    for (const auto& e : name.elements)
    {
        nh.nameElements.push_back(sourcetrail::NameElement{e.prefix, e.name, e.postfix});
    }
    return nh;
}

sourcetrail::SymbolKind translateSymbolKind(symhow::model::SymbolKind kind)
{
    using symhow::model::SymbolKind;
    switch (kind)
    {
        case SymbolKind::Namespace:
            return sourcetrail::SymbolKind::NAMESPACE;
        case SymbolKind::Struct:
            return sourcetrail::SymbolKind::STRUCT;
        case SymbolKind::Class:
            return sourcetrail::SymbolKind::CLASS;
        case SymbolKind::Union:
            return sourcetrail::SymbolKind::UNION;
        case SymbolKind::Enum:
            return sourcetrail::SymbolKind::ENUM;
        case SymbolKind::EnumConstant:
            return sourcetrail::SymbolKind::ENUM_CONSTANT;
        case SymbolKind::Field:
            return sourcetrail::SymbolKind::FIELD;
        case SymbolKind::GlobalVariable:
            return sourcetrail::SymbolKind::GLOBAL_VARIABLE;
        case SymbolKind::Function:
            return sourcetrail::SymbolKind::FUNCTION;
        case SymbolKind::Method:
            return sourcetrail::SymbolKind::METHOD;
        case SymbolKind::Typedef:
            return sourcetrail::SymbolKind::TYPEDEF;
        case SymbolKind::Macro:
            return sourcetrail::SymbolKind::MACRO;
        case SymbolKind::Type:
        case SymbolKind::Unknown:
        case SymbolKind::File:
        default:
            // Unknown/Type (and defensively, File, which should never reach
            // here -- see ensureSymbolNode()) fall back to TYPE: the
            // closest match in sourcetrail::SymbolKind, and -- crucially --
            // still a *real* kind, so the underlying node never stays
            // NodeKind::UNKNOWN for a symbol we chose to record.
            return sourcetrail::SymbolKind::TYPE;
    }
}

sourcetrail::ReferenceKind translateReferenceKind(symhow::model::ReferenceKind kind)
{
    using symhow::model::ReferenceKind;
    switch (kind)
    {
        case ReferenceKind::TypeUsage:
            return sourcetrail::ReferenceKind::TYPE_USAGE;
        case ReferenceKind::Usage:
            return sourcetrail::ReferenceKind::USAGE;
        case ReferenceKind::Call:
            return sourcetrail::ReferenceKind::CALL;
        case ReferenceKind::Inheritance:
            return sourcetrail::ReferenceKind::INHERITANCE;
        case ReferenceKind::Override:
            return sourcetrail::ReferenceKind::OVERRIDE;
        case ReferenceKind::Include:
            return sourcetrail::ReferenceKind::INCLUDE;
        case ReferenceKind::MacroUsage:
            return sourcetrail::ReferenceKind::MACRO_USAGE;
        case ReferenceKind::Unknown:
        default:
            return sourcetrail::ReferenceKind::USAGE;
    }
}

sourcetrail::DefinitionKind translateDefinitionKind(symhow::model::DefinitionKind kind)
{
    return kind == symhow::model::DefinitionKind::Explicit ? sourcetrail::DefinitionKind::EXPLICIT : sourcetrail::DefinitionKind::IMPLICIT;
}

int resolveLocal(const std::vector<int>& arr, symhow::model::LocalId id)
{
    return (id != symhow::model::kNoLocal && id < arr.size()) ? arr[id] : 0;
}

std::string joinIds(const std::unordered_set<int>& ids)
{
    std::ostringstream oss;
    bool first = true;
    for (int id : ids)
    {
        if (!first)
        {
            oss << ",";
        }
        oss << id;
        first = false;
    }
    return oss.str();
}

}  // namespace

SrctrldbBackend::SrctrldbBackend() = default;
SrctrldbBackend::~SrctrldbBackend() = default;

bool SrctrldbBackend::begin(const std::string& dbPath, std::string* err)
{
    m_dbPath = dbPath;
    m_writer = std::make_unique<sourcetrail::SourcetrailDBWriter>();

    if (!m_writer->open(dbPath))
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }

    // Opened here (schema now exists -- SourcetrailDBWriter::open() creates
    // it) rather than lazily on first checkpoint(), so IndexDriver can write
    // provenance via its own tiny WritableDb transaction on the SAME file
    // before startTransaction() -- deliberately NOT this m_sideDb instance
    // (kept exclusively for the checkpoint/finish flush path, never held
    // open across a consume() call).
    try
    {
        m_sideDb = std::make_unique<symhow::db::WritableDb>(dbPath, /*foreignKeys=*/false);
        m_sideDb->query("SELECT 1 FROM file LIMIT 1", [&](sqlite3_stmt*) { m_hasExistingFiles = true; });
        m_sideDb->query("SELECT 1 FROM sqlite_master WHERE type='table' AND name='symhow_file_fingerprint'",
            [&](sqlite3_stmt*) { m_hasExistingFingerprints = true; });
    }
    catch (const std::exception& e)
    {
        if (err != nullptr)
        {
            *err = e.what();
        }
        return false;
    }

    return true;
}

bool SrctrldbBackend::startTransaction(std::string* err)
{
    if (!m_writer->beginTransaction())
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }
    return true;
}

int SrctrldbBackend::ensureFileNodeByPath(const std::string& path, const std::string& language, bool indexed)
{
    auto it = m_fileNodeIdByPath.find(path);
    int id = 0;
    if (it != m_fileNodeIdByPath.end())
    {
        id = it->second;
    }
    else
    {
        id = m_writer->recordFile(path);
        if (id != 0)
        {
            m_writer->recordFileLanguage(id, language.empty() ? "cpp" : language);
            m_summary.files++;
        }
        m_fileNodeIdByPath.emplace(path, id);
        if (id != 0)
        {
            m_normalizedFileNodeId[symhow::common::paths::normalize(path)] = id;
        }
    }
    if (id != 0)
    {
        m_touchedSinceFlush.insert(id);
    }
    if (!indexed && id != 0)
    {
        m_nonIndexedFileIds.insert(id);
        m_anyNonIndexedFiles = true;
    }
    return id;
}

int SrctrldbBackend::ensureFileNode(const symhow::model::FileFact& fact)
{
    const int id = ensureFileNodeByPath(fact.path, fact.language, fact.indexed);
    if (id != 0 && fact.indexed)
    {
        auto [it, inserted] = m_fileFingerprints.emplace(id, fact.contentHash);
        if (inserted && m_hasExistingFiles)
        {
            // A surviving shared header may still have facts from TUs NOT
            // re-indexed this pass. Never certify new bytes over old/mixed
            // facts. Only a newly materialized file, or a surviving file
            // with an identical prior hash, can get a trusted fingerprint.
            const std::string sql = m_hasExistingFingerprints
                ? "SELECT fp.sha256 FROM file f LEFT JOIN symhow_file_fingerprint fp ON fp.file_node_id=f.id WHERE f.id=?1"
                : "SELECT NULL FROM file WHERE id=?1";
            m_sideDb->query(sql,
                [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, id); },
                [&](sqlite3_stmt* stmt) {
                    const char* previous = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
                    if (previous == nullptr || fact.contentHash != previous)
                    {
                        it->second.clear();
                    }
                });
        }
        if (inserted || it->second != fact.contentHash)
        {
            if (!inserted)
            {
                it->second.clear(); // mixed/unknown parsed bytes: never skip
            }
            m_pendingFileFingerprints.insert(id);
        }
    }
    return id;
}

int SrctrldbBackend::resolveTuFileReal(const symhow::model::TuFactBatch& batch, const std::vector<int>& fileReal)
{
    // batch.stats.tuFile is the CDB entry's `file` field (post-remap,
    // already normalized by IndexDriver before being stashed here), while
    // every FileFact.path comes from clang's own clang_getFileName() (see
    // clangutil::fileName()) and can still differ textually (e.g. clang
    // resolving a symlink the CDB spelled unresolved). A duplicate `file`
    // row would otherwise get minted here even though clang already
    // resolved the very same file for this batch -- so look for a batch
    // FileFact whose normalized path matches first, and only fall back to
    // minting a new one.
    const std::string normalizedTuFile = symhow::common::paths::normalize(batch.stats.tuFile);
    for (std::size_t i = 0; i < batch.files.size(); ++i)
    {
        if (symhow::common::paths::normalize(batch.files[i].path) == normalizedTuFile)
        {
            return fileReal[i];
        }
    }
    return ensureFileNodeByPath(normalizedTuFile, "cpp", /*indexed=*/true);
}

void SrctrldbBackend::bufferKindDef(int nodeId, symhow::model::SymbolKind kind, symhow::model::DefinitionKind definition)
{
    BufferedKindDef& buffered = m_pendingKindDef[nodeId];
    const int newRank = symhow::model::kindRank(kind);
    const int curRank = symhow::model::kindRank(buffered.kind);
    bool takeKind = newRank > curRank;
    if (!takeKind && newRank == curRank)
    {
        // Deterministic, order-INDEPENDENT tie-break on equal rank: compare
        // the actual stored-node-kind ints (the exact same total order the
        // kUpdateNodeTypeSql guard in flushPending() uses below), not
        // "first observed wins". Two equal-rank kinds that disagree (e.g.
        // CLASS vs STRUCT for a merged/collapsed name, or FUNCTION vs
        // METHOD) used to keep whichever TU's observation happened to reach
        // this node first -- which differs between a single full `symhow index`
        // session and a multi-session resume/`symhow update` that re-observes
        // the same node in a different TU order. Comparing the deterministic
        // kindOrder(kind) = (kindRank(kind), nodeKindInt(kind)) pair instead
        // makes the buffered (and, transitively, the persisted) kind a pure
        // function of the SET of observations, independent of order or
        // session boundaries.
        const int newKindInt = sourcetrail::nodeKindToInt(sourcetrail::symbolKindToNodeKind(translateSymbolKind(kind)));
        const int curKindInt = sourcetrail::nodeKindToInt(sourcetrail::symbolKindToNodeKind(translateSymbolKind(buffered.kind)));
        takeKind = newKindInt > curKindInt;
    }
    if (takeKind)
    {
        buffered.kind = kind;
    }
    if (static_cast<int>(definition) > static_cast<int>(buffered.definition))
    {
        buffered.definition = definition;
    }
}

int SrctrldbBackend::ensureSymbolNode(const symhow::model::SymbolFact& fact)
{
    if (fact.kind == symhow::model::SymbolKind::File)
    {
        // File "symbols" reuse the exact same node id as the corresponding
        // FileFact's own node (there is no separate sourcetrail::SymbolKind
        // for files; addFile() already sets NodeKind::FILE). fact.key IS
        // the file's absolute path by construction (see AstWalker's
        // fileSymbolId()); the FileFact for this path is always recorded
        // before AstWalker asks for the corresponding File-kind symbol, so
        // this should always be a cache hit in practice.
        auto it = m_byKey.find(fact.key);
        if (it != m_byKey.end())
        {
            m_touchedSinceFlush.insert(it->second);
            return it->second;
        }
        const int id = ensureFileNodeByPath(fact.key, "cpp", /*indexed=*/true);  // already inserts into m_touchedSinceFlush
        m_byKey.emplace(fact.key, id);
        return id;
    }

    auto it = m_byKey.find(fact.key);
    if (it != m_byKey.end())
    {
        bufferKindDef(it->second, fact.kind, fact.definition);
        m_touchedSinceFlush.insert(it->second);
        return it->second;
    }

    const sourcetrail::NameHierarchy nh = toNameHierarchy(fact.name);
    const std::string serialized = sourcetrail::serializeNameHierarchyToDatabaseString(nh);

    auto nameIt = m_byName.find(serialized);
    if (nameIt != m_byName.end())
    {
        // Residual collision: two distinct SymbolKeys serialize to the
        // same display name. The schema itself dedups nodes by
        // serialized_name, so the vendored writer would silently reuse the
        // same node anyway; make that explicit, count it, and warn.
        const int id = m_byKey.at(nameIt->second);
        m_byKey.emplace(fact.key, id);
        bufferKindDef(id, fact.kind, fact.definition);
        m_summary.nameCollisions++;
        if (m_verbose)
        {
            std::fprintf(
                stderr,
                "symhow index: warning: symbol name collision -- keys '%s' and '%s' both serialize to '%s'; merged into node %d\n",
                nameIt->second.c_str(),
                fact.key.c_str(),
                serialized.c_str(),
                id);
        }
        m_touchedSinceFlush.insert(id);
        return id;
    }

    const int id = m_writer->recordSymbol(nh);
    m_byKey.emplace(fact.key, id);
    m_byName.emplace(serialized, fact.key);
    bufferKindDef(id, fact.kind, fact.definition);
    m_summary.symbols++;
    m_touchedSinceFlush.insert(id);
    return id;
}

int SrctrldbBackend::ensureLocalSymbolNode(const std::string& name)
{
    auto it = m_localSymbolIdByName.find(name);
    if (it != m_localSymbolIdByName.end())
    {
        return it->second;
    }
    const int id = m_writer->recordLocalSymbol(name);
    m_localSymbolIdByName.emplace(name, id);
    return id;
}

int SrctrldbBackend::ensureReference(int ctxReal, int targetReal, symhow::model::ReferenceKind kind)
{
    const RefTripleKey key{ctxReal, targetReal, static_cast<int>(kind)};
    auto it = m_refByTriple.find(key);
    if (it != m_refByTriple.end())
    {
        return it->second;
    }
    const int id = m_writer->recordReference(ctxReal, targetReal, translateReferenceKind(kind));
    m_refByTriple.emplace(key, id);
    m_summary.references++;
    return id;
}

bool SrctrldbBackend::consume(const symhow::model::TuFactBatch& batch, std::string* err)
{
    if (!m_writer)
    {
        if (err != nullptr)
        {
            *err = "consume() called before begin()";
        }
        return false;
    }

    std::vector<int> fileReal(batch.files.size());
    for (std::size_t i = 0; i < batch.files.size(); ++i)
    {
        fileReal[i] = ensureFileNode(batch.files[i]);
    }

    std::vector<int> symReal(batch.symbols.size());
    for (std::size_t i = 0; i < batch.symbols.size(); ++i)
    {
        symReal[i] = ensureSymbolNode(batch.symbols[i]);
        const int symId = symReal[i];
        if (symId != 0)
        {
            for (const symhow::model::LocalId declFile : batch.symbols[i].declFiles)
            {
                const int fileId = resolveLocal(fileReal, declFile);
                if (fileId != 0)
                {
                    m_pendingSymbolDef.emplace(symId, fileId);
                }
            }
        }
    }

    std::vector<int> localReal(batch.localSymbols.size());
    for (std::size_t i = 0; i < batch.localSymbols.size(); ++i)
    {
        localReal[i] = ensureLocalSymbolNode(batch.localSymbols[i]);
    }

    struct RefState
    {
        bool solved;
        int realId;
        symhow::model::ReferenceKind kind;
        int ctxReal;
    };
    std::vector<RefState> refStates(batch.references.size());
    for (std::size_t i = 0; i < batch.references.size(); ++i)
    {
        const symhow::model::ReferenceFact& rf = batch.references[i];
        const int ctxReal = resolveLocal(symReal, rf.context);
        if (rf.target != symhow::model::kNoLocal)
        {
            const int tgtReal = resolveLocal(symReal, rf.target);
            refStates[i] = RefState{true, ensureReference(ctxReal, tgtReal, rf.kind), rf.kind, ctxReal};
        }
        else
        {
            // Unsolved: recordReferenceToUnsolvedSymhol needs a location up
            // front, so materialization is deferred to the matching
            // occurrence below (AstWalker always pairs unsolvedReference()
            // with exactly one occurrence() call).
            refStates[i] = RefState{false, 0, rf.kind, ctxReal};
        }
    }

    for (const symhow::model::OccurrenceFact& occ : batch.occurrences)
    {
        const sourcetrail::SourceRange sr{
            resolveLocal(fileReal, occ.range.file),
            static_cast<int>(occ.range.startLine),
            static_cast<int>(occ.range.startCol),
            static_cast<int>(occ.range.endLine),
            static_cast<int>(occ.range.endCol)};

        switch (occ.cls)
        {
            case symhow::model::ElementClass::Symbol:
            {
                const int id = resolveLocal(symReal, occ.element);
                switch (occ.kind)
                {
                    case symhow::model::LocationKind::Token:
                        m_writer->recordSymbolLocation(id, sr);
                        break;
                    case symhow::model::LocationKind::Scope:
                        m_writer->recordSymbolScopeLocation(id, sr);
                        break;
                    case symhow::model::LocationKind::Signature:
                        m_writer->recordSymbolSignatureLocation(id, sr);
                        break;
                    case symhow::model::LocationKind::Qualifier:
                        m_writer->recordQualifierLocation(id, sr);
                        break;
                    default:
                        break;
                }
                break;
            }
            case symhow::model::ElementClass::LocalSymbol:
            {
                const int id = resolveLocal(localReal, occ.element);
                m_writer->recordLocalSymbolLocation(id, sr);
                break;
            }
            case symhow::model::ElementClass::Reference:
            {
                if (occ.element >= refStates.size())
                {
                    break;
                }
                RefState& state = refStates[occ.element];
                if (state.solved)
                {
                    m_writer->recordReferenceLocation(state.realId, sr);
                }
                else
                {
                    state.realId = m_writer->recordReferenceToUnsolvedSymhol(state.ctxReal, translateReferenceKind(state.kind), sr);
                    state.solved = true;
                    m_sawUnsolvedReference = true;
                    m_summary.references++;
                }
                break;
            }
        }
    }

    int tuFileReal = 0;
    for (const symhow::model::ErrorFact& e : batch.errors)
    {
        sourcetrail::SourceRange sr{0, 1, 1, 1, 1};
        if (e.hasRange)
        {
            sr = sourcetrail::SourceRange{
                resolveLocal(fileReal, e.range.file),
                static_cast<int>(e.range.startLine),
                static_cast<int>(e.range.startCol),
                static_cast<int>(e.range.endLine),
                static_cast<int>(e.range.endCol)};
        }
        else if (!batch.stats.tuFile.empty())
        {
            // Attach range-less errors (e.g. a sanitize()/parse failure with
            // no diagnostic location) to the TU's own main file node rather
            // than an arbitrary first file of the batch -- never 0.
            if (tuFileReal == 0)
            {
                tuFileReal = resolveTuFileReal(batch, fileReal);
            }
            sr.fileId = tuFileReal;
        }
        m_writer->recordError(e.message, e.fatal, sr);
        m_summary.errors++;
    }

    return true;
}

void SrctrldbBackend::flushPending(symhow::db::WritableDb& db, const std::vector<PendingTuRow>& tuRows)
{
    db.exec(kSymhowFileFingerprintDdl);
    // Non-indexed files touched since the last flush: the vendored writer
    // always creates a `file` row as indexed=1/complete=1 (recordFile()
    // has no concept of "non-indexed"), so every file this pass recorded
    // but marked non-indexed (FileFact::indexed == false) needs this
    // correction. Applied on EVERY flush (not just finish()'s final one):
    // if this file was only ever touched by a TU in an already-checkpointed
    // (or interrupted-and-resumed) chunk, its correction must land THEN,
    // not stay pending until a finish() that might not run in this process
    // at all (see finishInterrupted()).
    if (!m_nonIndexedFileIds.empty())
    {
        const std::string idList = joinIds(m_nonIndexedFileIds);
        db.exec("UPDATE file SET indexed = 0, complete = 0 WHERE id IN (" + idList + ");");
        db.exec("DELETE FROM filecontent WHERE id IN (" + idList + ");");
        db.exec("DELETE FROM symhow_file_fingerprint WHERE file_node_id IN (" + idList + ");");
        m_nonIndexedFileIds.clear();
    }

    // --- symhow-private side tables (Phase 1: incremental-update groundwork) ---
    // IF NOT EXISTS: cheap and idempotent, so safe to redo on every
    // checkpoint (in case this is the very first flush of the session).
    db.exec(
        "CREATE TABLE IF NOT EXISTS symhow_symbol_def("
        "node_id INTEGER NOT NULL, "
        "file_node_id INTEGER NOT NULL, "
        "PRIMARY KEY(node_id, file_node_id)"
        ") WITHOUT ROWID;");
    db.exec(kSymhowTuDdl);

    // Persist only this checkpoint's delta, from the bytes libclang parsed,
    // never by re-reading disk after parsing (especially important for -j).
    // Fingerprints and symhow_tu status land in the same checkpoint transaction.
    for (int id : m_pendingFileFingerprints)
    {
        const std::string& hash = m_fileFingerprints.at(id);
        db.execBound("DELETE FROM symhow_file_fingerprint WHERE file_node_id=?1",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, id); });
        if (!hash.empty())
        {
            db.execBound(
                "INSERT INTO symhow_file_fingerprint(file_node_id, sha256) "
                "SELECT id, ?2 FROM file WHERE id=?1 AND indexed=1 AND complete=1",
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_int(stmt, 1, id);
                    sqlite3_bind_text(stmt, 2, hash.c_str(), -1, SQLITE_TRANSIENT);
                });
        }
    }
    m_pendingFileFingerprints.clear();

    // symhow_touched: every node id THIS indexing pass created or looked up
    // (symbols via m_byKey, files via m_fileNodeIdByPath). Describes the
    // most recent pass -- for a full `symhow index` that is simply "every node
    // in the database", for a partial re-index (`symhow update`, or resuming an
    // interrupted `symhow index` via `symhow update`) it is exactly the set of
    // nodes the re-indexed TUs still produce. Invalidator's post-index
    // sweep needs that distinction: a node with no occurrence and no
    // non-MEMBER edge is indistinguishable from a legitimate implicit-
    // instantiation node a fresh index keeps, UNLESS you know whether the
    // pass that just ran still produces it. invalidate() empties this
    // table before the re-index, so a stale entry can never protect a dead
    // node.
    //
    // DROP+recreate happens exactly ONCE per session (the first flush --
    // checkpoint() or finish(), whichever runs first), not on every
    // checkpoint: symhow_touched must describe the WHOLE session's output (every
    // checkpoint's nodes), not just the latest chunk's -- dropping it on
    // every checkpoint would un-protect nodes from earlier, already-
    // committed chunks of the SAME run.
    if (!m_symhowTouchedInitialized)
    {
        db.exec("DROP TABLE IF EXISTS symhow_touched;");
        db.exec("CREATE TABLE symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;");
        m_symhowTouchedInitialized = true;
    }
    {
        static const char* kInsertTouchedSql = "INSERT OR IGNORE INTO symhow_touched(node_id) VALUES(?1)";
        for (int nodeId : m_touchedSinceFlush)
        {
            if (nodeId == 0)
            {
                continue;
            }
            db.execBound(kInsertTouchedSql, [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, nodeId); });
        }
        m_touchedSinceFlush.clear();

        // Ancestor closure over MEMBER edges: the vendored writer's
        // addNodeHierarchy() materializes every INTERMEDIATE name element of
        // a NameHierarchy as its own node but only ever hands back the
        // LEAF's id, so a purely structural scope node (e.g. the anonymous
        // scopes around a libc `timespec`) can be created by this pass
        // without ever passing through m_byKey. A node whose child this
        // pass produced was produced by this pass too, by definition. Rerun
        // on every flush (idempotent, INSERT OR IGNORE) since a later chunk
        // can add new leaves under an already-touched ancestor.
        for (;;)
        {
            db.exec(
                "INSERT OR IGNORE INTO symhow_touched(node_id) SELECT e.source_node_id FROM edge e "
                "JOIN symhow_touched t ON t.node_id = e.target_node_id WHERE e.type = 1;");
            if (db.changes() <= 0)
            {
                break;
            }
        }

        if (m_sawUnsolvedReference)
        {
            // The writer's shared "unsolved symbol" node (see
            // SourcetrailDBWriter::recordReferenceToUnsolvedSymhol) is
            // created inside the vendored writer and never passes through
            // m_byKey. Matched on its exact serialized name -- char(9) is
            // the '\t' separator NameHierarchy serializes with. Only when
            // this pass actually produced an unsolved reference: if it did
            // not, the node is NOT this pass's output and must stay
            // collectable like any other.
            db.exec(
                "INSERT OR IGNORE INTO symhow_touched(node_id) SELECT id FROM node "
                "WHERE serialized_name = char(9) || 'munsolved symbol' || char(9) || 's' || char(9) || 'p';");
        }
    }

    // --- node.type / symbol.definition_kind: upgrade-only writes -------
    //
    // Rank the STORED node-kind ints (not translateSymbolKind()'s output --
    // that maps BOTH Unknown and Type to sourcetrail::SymbolKind::TYPE, so
    // ranking against it made the ladder degenerate: every freshly-created
    // node, which the vendored writer always creates as NodeKind::UNKNOWN
    // (see addNodeHierarchy()), fell into the CASE's ELSE arm and was never
    // upgraded). rank(UNKNOWN) = 0 matches kindRank(SymbolKind::Unknown) =
    // 0, and rank(TYPE) = 1 matches kindRank(SymbolKind::Type) = 1, so a
    // pending kind's kindRank() lines up exactly with the stored node.type's
    // rank.
    const int unknownNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::NodeKind::UNKNOWN);
    const int typeNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::NodeKind::TYPE);

    // Strictly-upgrading, and now fully order-INDEPENDENT (including
    // between equal-ranked kinds): a stored kind is only replaced by one
    // that's STRICTLY greater under kindOrder(kind) = (rank(kind),
    // nodeKindInt(kind)) -- rank compared first, and on a RANK TIE, the
    // stored node-kind int itself (`type`) is compared against the new
    // kind's int, with the new kind winning only if it's numerically
    // greater. This subsumes the old "OR type = UNKNOWN" special case for
    // a virgin node (UNKNOWN's int is strictly less than every other
    // NodeKind int, including TYPE's, so a rank-0-vs-rank-0 tie against a
    // virgin UNKNOWN node still upgrades it) without needing it spelled out
    // separately.
    //
    // This used to be a plain `<` (never re-decide a rank tie at all, i.e.
    // "first real kind wins"): libstdc++'s `std::pair` is reached both as a
    // ClassTemplate (-> CLASS, rank 2) and as a StructDecl (-> STRUCT, rank
    // 2), so which TU happened to observe it FIRST decided node.type -- and
    // "first" differs between a single full `symhow index` session and a
    // multi-session resume/`symhow update` that re-observes the same node in a
    // different TU order (caught by `symhow dbdiff` between a fresh 200-TU
    // index and a 26+174-TU `symhow update` resume on the cucs corpus).
    // Comparing `type < ?1` (the numeric NodeKind int) on a rank tie
    // instead makes the final answer a pure function of the SET of
    // observed kinds across the whole run, independent of which session or
    // TU order produced them -- because this same guard is applied
    // identically whether it runs ONCE (commitEvery=0) or once per
    // checkpoint/session against progressively-updated stored state, the
    // node.type this converges to after ALL observations have been applied
    // (in any order, across any number of sessions) is always the same
    // value: the observed kind with the numerically greatest NodeKind int
    // among those with the highest rank.
    static const char* kUpdateNodeTypeSql =
        "UPDATE node SET type = ?1 "
        "WHERE id = ?2 AND ("
        "  (CASE type WHEN ?3 THEN 0 WHEN ?4 THEN 1 ELSE 2 END) < ?5 "
        "  OR ((CASE type WHEN ?3 THEN 0 WHEN ?4 THEN 1 ELSE 2 END) = ?5 AND type < ?1)"
        ")";
    static const char* kUpsertSymbolSql =
        "INSERT INTO symbol(id, definition_kind) VALUES(?1, ?2) "
        "ON CONFLICT(id) DO UPDATE SET definition_kind = max(definition_kind, excluded.definition_kind)";

    for (const auto& [nodeId, kd] : m_pendingKindDef)
    {
        // No "newRank > 0" skip here: even a pending Unknown kind (rank 0)
        // must still be allowed to upgrade a virgin UNKNOWN node (rank 0) to
        // TYPE, matching the old unconditional recordSymbolKind() behavior
        // -- the WHERE clause's own rank(stored) <= newRank guard is what
        // keeps a real kind from ever being downgraded back to TYPE/UNKNOWN.
        {
            const int newNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::symbolKindToNodeKind(translateSymbolKind(kd.kind)));
            const int newRank = symhow::model::kindRank(kd.kind);
            db.execBound(
                kUpdateNodeTypeSql,
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_int(stmt, 1, newNodeKindInt);
                    sqlite3_bind_int(stmt, 2, nodeId);
                    sqlite3_bind_int(stmt, 3, unknownNodeKindInt);
                    sqlite3_bind_int(stmt, 4, typeNodeKindInt);
                    sqlite3_bind_int(stmt, 5, newRank);
                });
        }

        if (kd.definition != symhow::model::DefinitionKind::None)
        {
            const int dkInt = sourcetrail::definitionKindToInt(translateDefinitionKind(kd.definition));
            db.execBound(
                kUpsertSymbolSql,
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_int(stmt, 1, nodeId);
                    sqlite3_bind_int(stmt, 2, dkInt);
                });
        }
    }
    m_pendingKindDef.clear();

    static const char* kInsertSymbolDefSql = "INSERT OR IGNORE INTO symhow_symbol_def(node_id, file_node_id) VALUES(?1, ?2)";
    for (const auto& [nodeId, fileId] : m_pendingSymbolDef)
    {
        db.execBound(
            kInsertSymbolDefSql,
            [&](sqlite3_stmt* stmt) {
                sqlite3_bind_int(stmt, 1, nodeId);
                sqlite3_bind_int(stmt, 2, fileId);
            });
    }
    m_pendingSymbolDef.clear();

    // --- symhow_tu: one upsert per PATH whose LAST entry was consumed since
    // the last flush ----------------------------------------------------
    // `tuRows` only ever contains one row per path that actually completed
    // (IndexDriver only pushes a PendingTuRow once a path's last CDB entry,
    // in CDB order, has been consumed -- see remainingEntriesByPath there),
    // so an interrupted run that stopped mid-path never reaches this UPSERT
    // for that path at all -- see PendingTuRow's doc comment and
    // docs/KNOWN_LIMITATIONS.md's "Phase 9" section. Still idempotent
    // (ON CONFLICT DO UPDATE) since a `symhow update` re-index of an
    // already-indexed path legitimately reaches this UPSERT again.
    static const char* kUpsertTuSql =
        "INSERT INTO symhow_tu(path, args_hash, file_node_id, status, entry_count) VALUES(?1, ?2, ?3, ?4, ?5) "
        "ON CONFLICT(path) DO UPDATE SET args_hash = excluded.args_hash, file_node_id = excluded.file_node_id, "
        "status = excluded.status, entry_count = excluded.entry_count";
    for (const PendingTuRow& row : tuRows)
    {
        const std::string normalizedPath = symhow::common::paths::normalize(row.path);
        const auto it = m_normalizedFileNodeId.find(normalizedPath);
        db.execBound(
            kUpsertTuSql,
            [&](sqlite3_stmt* stmt) {
                sqlite3_bind_text(stmt, 1, row.path.c_str(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(stmt, 2, row.argsHash.c_str(), -1, SQLITE_TRANSIENT);
                if (it != m_normalizedFileNodeId.end())
                {
                    sqlite3_bind_int(stmt, 3, it->second);
                }
                else
                {
                    sqlite3_bind_null(stmt, 3);
                }
                sqlite3_bind_text(stmt, 4, row.status.c_str(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_int(stmt, 5, static_cast<int>(row.entryCount));
            });
    }
}

bool SrctrldbBackend::commitAndFlush(const std::vector<PendingTuRow>& tuRows, std::string* err)
{
    if (!m_writer)
    {
        if (err != nullptr)
        {
            *err = "checkpoint()/finish() called before begin()";
        }
        return false;
    }

    // NOTE: kind/definition-kind are NO LONGER pushed through the vendored
    // writer here (recordSymbolKind unconditionally overwrites node.type;
    // recordSymbolDefinitionKind is first-write-wins via INSERT OR IGNORE --
    // neither behavior is safe for a future incremental `symhow update` that
    // re-consumes a symbol observed differently across runs). Instead
    // they're applied via flushPending()/our own WritableDb, with an
    // explicit never-downgrade guard on node.type and a max()-upgrade on
    // definition_kind.
    if (!m_writer->commitTransaction())
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }

    try
    {
        m_sideDb->begin();
        flushPending(*m_sideDb, tuRows);
        m_sideDb->commit();
    }
    catch (const std::exception& e)
    {
        if (err != nullptr)
        {
            *err = e.what();
        }
        return false;
    }

    return true;
}

bool SrctrldbBackend::checkpoint(const std::vector<PendingTuRow>& tuRows, std::string* err)
{
    if (!commitAndFlush(tuRows, err))
    {
        return false;
    }
    return startTransaction(err);
}

bool SrctrldbBackend::finishInterrupted(const std::vector<PendingTuRow>& tuRows, std::string* err)
{
    if (!commitAndFlush(tuRows, err))
    {
        return false;
    }
    if (!m_writer->close())
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }
    m_writer.reset();
    m_sideDb.reset();
    // Deliberately: no reader indices, no ANALYZE/VACUUM, and
    // symhow_index_in_progress is left set -- finish() (via a later `symhow update`)
    // does all of that.
    return true;
}

bool SrctrldbBackend::finish(symhow::model::IndexSummary& summary, std::string* err)
{
    // By the time finish() runs, the caller has already checkpoint()ed
    // everything (the LAST checkpoint() call, covering whatever wasn't
    // flushed yet, is the caller's job -- for commitEvery=0 that's the
    // ONLY checkpoint() call, covering the entire run): every buffer here
    // is empty and every symhow_tu row already written. finish() just needs to
    // close out the writer's (now-empty) transaction and do the one-time,
    // end-of-run-only work: reader indices, non-indexed-file cleanup,
    // ANALYZE/VACUUM, and clearing symhow_index_in_progress.
    if (!m_writer)
    {
        if (err != nullptr)
        {
            *err = "finish() called before begin()";
        }
        return false;
    }

    if (!m_writer->commitTransaction())
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }
    if (!m_writer->close())
    {
        if (err != nullptr)
        {
            *err = m_writer->getLastError();
        }
        return false;
    }
    m_writer.reset();

    // Post-write correction + reader indices, via our own raw sqlite3
    // handle (symhow::db::WritableDb) -- never patch the vendored writer to do
    // this instead. Reuses m_sideDb (already open) rather than a fresh
    // connection.
    try
    {
        symhow::db::WritableDb& db = *m_sideDb;
        db.begin();

        // Any non-indexed files from THIS (final) checkpoint's own
        // delta -- flushPending() already handled every prior checkpoint's,
        // but the caller's mandatory last checkpoint() call (see finish()'s
        // class comment) already drained m_nonIndexedFileIds too, so this
        // is normally a no-op; kept for defensiveness (a caller that skips
        // the pre-finish() checkpoint() entirely, e.g. a future direct
        // caller, still gets correct behavior).
        if (!m_nonIndexedFileIds.empty())
        {
            const std::string idList = joinIds(m_nonIndexedFileIds);
            db.exec("UPDATE file SET indexed = 0, complete = 0 WHERE id IN (" + idList + ");");
            db.exec("DELETE FROM filecontent WHERE id IN (" + idList + ");");
            m_nonIndexedFileIds.clear();
        }

        symhow::db::deleteMeta(db, "symhow_index_in_progress");

        static const char* kIndexStatements[] = {
            "CREATE INDEX IF NOT EXISTS symhow_edge_source_idx ON edge(source_node_id);",
            "CREATE INDEX IF NOT EXISTS symhow_edge_target_idx ON edge(target_node_id);",
            "CREATE INDEX IF NOT EXISTS symhow_edge_type_idx ON edge(type);",
            "CREATE INDEX IF NOT EXISTS symhow_node_type_idx ON node(type);",
            "CREATE INDEX IF NOT EXISTS symhow_srcloc_file_idx ON source_location(file_node_id);",
            "CREATE INDEX IF NOT EXISTS symhow_occurrence_elem_idx ON occurrence(element_id);",
            "CREATE INDEX IF NOT EXISTS symhow_occurrence_loc_idx ON occurrence(source_location_id);",
            "CREATE INDEX IF NOT EXISTS symhow_file_path_idx ON file(path);",
            "CREATE INDEX IF NOT EXISTS symhow_symbol_defkind_idx ON symbol(definition_kind);",
        };
        for (const char* stmt : kIndexStatements)
        {
            db.exec(stmt);
        }

        db.commit();

        // ANALYZE/VACUUM are best-effort maintenance, not correctness --
        // a fully-written, already-committed database must not be treated
        // as a failed `symhow index` run just because one of these throws (e.g.
        // VACUUM needing free disk space it doesn't have). Downgrade each to
        // an independent stderr warning instead, restoring the previous
        // ignore-result behavior.
        try
        {
            db.exec("ANALYZE;");
        }
        catch (const std::exception& e)
        {
            std::fprintf(stderr, "symhow index: warning: ANALYZE failed: %s\n", e.what());
        }
        if (!m_noVacuum && m_anyNonIndexedFiles)
        {
            try
            {
                db.exec("VACUUM;");
            }
            catch (const std::exception& e)
            {
                std::fprintf(stderr, "symhow index: warning: VACUUM failed: %s\n", e.what());
            }
        }
    }
    catch (const std::exception& e)
    {
        if (err != nullptr)
        {
            *err = e.what();
        }
        return false;
    }

    m_sideDb.reset();

    summary.symbols = m_summary.symbols;
    summary.references = m_summary.references;
    summary.files = m_summary.files;
    summary.errors = m_summary.errors;
    summary.nameCollisions = m_summary.nameCollisions;
    return true;
}

}  // namespace symhow::index
