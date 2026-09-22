#include "kg/index/SrctrldbBackend.h"

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

#include "kg/common/Paths.h"
#include "kg/db/WritableDb.h"
#include "kg/index/Provenance.h"

namespace kg::index
{

namespace
{

sourcetrail::NameHierarchy toNameHierarchy(const kg::model::Name& name)
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

sourcetrail::SymbolKind translateSymbolKind(kg::model::SymbolKind kind)
{
    using kg::model::SymbolKind;
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

sourcetrail::ReferenceKind translateReferenceKind(kg::model::ReferenceKind kind)
{
    using kg::model::ReferenceKind;
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

sourcetrail::DefinitionKind translateDefinitionKind(kg::model::DefinitionKind kind)
{
    return kind == kg::model::DefinitionKind::Explicit ? sourcetrail::DefinitionKind::EXPLICIT : sourcetrail::DefinitionKind::IMPLICIT;
}

int resolveLocal(const std::vector<int>& arr, kg::model::LocalId id)
{
    return (id != kg::model::kNoLocal && id < arr.size()) ? arr[id] : 0;
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
    }
    if (!indexed && id != 0)
    {
        m_nonIndexedFileIds.insert(id);
    }
    return id;
}

int SrctrldbBackend::ensureFileNode(const kg::model::FileFact& fact)
{
    return ensureFileNodeByPath(fact.path, fact.language, fact.indexed);
}

int SrctrldbBackend::resolveTuFileReal(const kg::model::TuFactBatch& batch, const std::vector<int>& fileReal)
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
    const std::string normalizedTuFile = kg::common::paths::normalize(batch.stats.tuFile);
    for (std::size_t i = 0; i < batch.files.size(); ++i)
    {
        if (kg::common::paths::normalize(batch.files[i].path) == normalizedTuFile)
        {
            return fileReal[i];
        }
    }
    return ensureFileNodeByPath(normalizedTuFile, "cpp", /*indexed=*/true);
}

void SrctrldbBackend::bufferKindDef(int nodeId, kg::model::SymbolKind kind, kg::model::DefinitionKind definition)
{
    BufferedKindDef& buffered = m_pendingKindDef[nodeId];
    if (kg::model::kindRank(kind) > kg::model::kindRank(buffered.kind))
    {
        buffered.kind = kind;
    }
    if (static_cast<int>(definition) > static_cast<int>(buffered.definition))
    {
        buffered.definition = definition;
    }
}

int SrctrldbBackend::ensureSymbolNode(const kg::model::SymbolFact& fact)
{
    if (fact.kind == kg::model::SymbolKind::File)
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
            return it->second;
        }
        const int id = ensureFileNodeByPath(fact.key, "cpp", /*indexed=*/true);
        m_byKey.emplace(fact.key, id);
        return id;
    }

    auto it = m_byKey.find(fact.key);
    if (it != m_byKey.end())
    {
        bufferKindDef(it->second, fact.kind, fact.definition);
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
                "kg index: warning: symbol name collision -- keys '%s' and '%s' both serialize to '%s'; merged into node %d\n",
                nameIt->second.c_str(),
                fact.key.c_str(),
                serialized.c_str(),
                id);
        }
        return id;
    }

    const int id = m_writer->recordSymbol(nh);
    m_byKey.emplace(fact.key, id);
    m_byName.emplace(serialized, fact.key);
    bufferKindDef(id, fact.kind, fact.definition);
    m_summary.symbols++;
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

int SrctrldbBackend::ensureReference(int ctxReal, int targetReal, kg::model::ReferenceKind kind)
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

bool SrctrldbBackend::consume(const kg::model::TuFactBatch& batch, std::string* err)
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
            for (const kg::model::LocalId declFile : batch.symbols[i].declFiles)
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
        kg::model::ReferenceKind kind;
        int ctxReal;
    };
    std::vector<RefState> refStates(batch.references.size());
    for (std::size_t i = 0; i < batch.references.size(); ++i)
    {
        const kg::model::ReferenceFact& rf = batch.references[i];
        const int ctxReal = resolveLocal(symReal, rf.context);
        if (rf.target != kg::model::kNoLocal)
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

    for (const kg::model::OccurrenceFact& occ : batch.occurrences)
    {
        const sourcetrail::SourceRange sr{
            resolveLocal(fileReal, occ.range.file),
            static_cast<int>(occ.range.startLine),
            static_cast<int>(occ.range.startCol),
            static_cast<int>(occ.range.endLine),
            static_cast<int>(occ.range.endCol)};

        switch (occ.cls)
        {
            case kg::model::ElementClass::Symbol:
            {
                const int id = resolveLocal(symReal, occ.element);
                switch (occ.kind)
                {
                    case kg::model::LocationKind::Token:
                        m_writer->recordSymbolLocation(id, sr);
                        break;
                    case kg::model::LocationKind::Scope:
                        m_writer->recordSymbolScopeLocation(id, sr);
                        break;
                    case kg::model::LocationKind::Signature:
                        m_writer->recordSymbolSignatureLocation(id, sr);
                        break;
                    case kg::model::LocationKind::Qualifier:
                        m_writer->recordQualifierLocation(id, sr);
                        break;
                    default:
                        break;
                }
                break;
            }
            case kg::model::ElementClass::LocalSymbol:
            {
                const int id = resolveLocal(localReal, occ.element);
                m_writer->recordLocalSymbolLocation(id, sr);
                break;
            }
            case kg::model::ElementClass::Reference:
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
    for (const kg::model::ErrorFact& e : batch.errors)
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

bool SrctrldbBackend::finish(kg::model::IndexSummary& summary, std::string* err)
{
    if (!m_writer)
    {
        if (err != nullptr)
        {
            *err = "finish() called before begin()";
        }
        return false;
    }

    // NOTE: kind/definition-kind are NO LONGER pushed through the vendored
    // writer here (recordSymbolKind unconditionally overwrites node.type;
    // recordSymbolDefinitionKind is first-write-wins via INSERT OR IGNORE --
    // neither behavior is safe for a future incremental `kg update` that
    // re-consumes a symbol observed differently across runs). Instead they're
    // applied below, via our own WritableDb, with an explicit
    // never-downgrade guard on node.type and a max()-upgrade on
    // definition_kind.

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
    // handle (kg::db::WritableDb) -- never patch the vendored writer to do
    // this instead.
    try
    {
        kg::db::WritableDb db(m_dbPath, /*foreignKeys=*/false);
        db.begin();

        if (!m_nonIndexedFileIds.empty())
        {
            const std::string idList = joinIds(m_nonIndexedFileIds);
            db.exec("UPDATE file SET indexed = 0, complete = 0 WHERE id IN (" + idList + ");");
            db.exec("DELETE FROM filecontent WHERE id IN (" + idList + ");");
        }

        // --- kg-private side tables (Phase 1: incremental-update groundwork) ---
        db.exec(
            "CREATE TABLE IF NOT EXISTS kg_symbol_def("
            "node_id INTEGER NOT NULL, "
            "file_node_id INTEGER NOT NULL, "
            "PRIMARY KEY(node_id, file_node_id)"
            ") WITHOUT ROWID;");
        db.exec(kKgTuDdl);

        // kg_touched: every node id THIS indexing pass created or looked
        // up (symbols via m_byKey, files via m_fileNodeIdByPath). Written
        // unconditionally, so it always describes the most recent pass --
        // for a full `kg index` that is simply "every node in the
        // database", for the partial re-index `kg update` runs it is
        // exactly the set of nodes the re-indexed TUs still produce.
        // Invalidator's post-index sweep needs that distinction: a node
        // with no occurrence and no non-MEMBER edge is indistinguishable
        // from a legitimate implicit-instantiation node a fresh index
        // keeps, UNLESS you know whether the pass that just ran still
        // produces it. invalidate() empties this table before the
        // re-index, so a stale entry can never protect a dead node.
        db.exec("DROP TABLE IF EXISTS kg_touched;");
        db.exec("CREATE TABLE kg_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;");
        {
            static const char* kInsertTouchedSql = "INSERT OR IGNORE INTO kg_touched(node_id) VALUES(?1)";
            const auto insertTouched = [&](int nodeId) {
                if (nodeId == 0)
                {
                    return;
                }
                db.execBound(kInsertTouchedSql, [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, nodeId); });
            };
            for (const auto& [key, nodeId] : m_byKey)
            {
                insertTouched(nodeId);
            }
            for (const auto& [path, nodeId] : m_fileNodeIdByPath)
            {
                insertTouched(nodeId);
            }
            // Ancestor closure over MEMBER edges: the vendored writer's
            // addNodeHierarchy() materializes every INTERMEDIATE name
            // element of a NameHierarchy as its own node but only ever
            // hands back the LEAF's id, so a purely structural scope node
            // (e.g. the anonymous scopes around a libc `timespec`) can be
            // created by this pass without ever passing through m_byKey.
            // A node whose child this pass produced was produced by this
            // pass too, by definition.
            for (;;)
            {
                db.exec(
                    "INSERT OR IGNORE INTO kg_touched(node_id) SELECT e.source_node_id FROM edge e "
                    "JOIN kg_touched t ON t.node_id = e.target_node_id WHERE e.type = 1;");
                if (db.changes() <= 0)
                {
                    break;
                }
            }

            if (m_sawUnsolvedReference)
            {
                // The writer's shared "unsolved symbol" node (see
                // SourcetrailDBWriter::recordReferenceToUnsolvedSymhol) is
                // created inside the vendored writer and never passes
                // through m_byKey. Matched on its exact serialized name --
                // char(9) is the '\t' separator NameHierarchy serializes
                // with. Only when this pass actually produced an unsolved
                // reference: if it did not, the node is NOT this pass's
                // output and must stay collectable like any other.
                db.exec(
                    "INSERT OR IGNORE INTO kg_touched(node_id) SELECT id FROM node "
                    "WHERE serialized_name = char(9) || 'munsolved symbol' || char(9) || 's' || char(9) || 'p';");
            }
        }

        // --- node.type / symbol.definition_kind: upgrade-only writes -------
        //
        // Rank the STORED node-kind ints (not translateSymbolKind()'s
        // output -- that maps BOTH Unknown and Type to sourcetrail::
        // SymbolKind::TYPE, so ranking against it made the ladder
        // degenerate: every freshly-created node, which the vendored writer
        // always creates as NodeKind::UNKNOWN (see addNodeHierarchy()), fell
        // into the CASE's ELSE arm and was never upgraded). rank(UNKNOWN) =
        // 0 matches kindRank(SymbolKind::Unknown) = 0, and rank(TYPE) = 1
        // matches kindRank(SymbolKind::Type) = 1, so a pending kind's
        // kindRank() lines up exactly with the stored node.type's rank.
        const int unknownNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::NodeKind::UNKNOWN);
        const int typeNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::NodeKind::TYPE);

        // Strictly-upgrading, order-INDEPENDENT: a stored kind is only
        // replaced by a STRICTLY higher-ranked one (plus the one special
        // case of a virgin UNKNOWN node, which must still be allowed to
        // become TYPE even though both rank 0). `<=` here instead of `<`
        // made EQUAL-ranked kinds last-writer-wins, i.e. dependent on TU
        // processing order: libstdc++'s `std::pair` is reached both as a
        // ClassTemplate (-> CLASS, rank 2) and as a StructDecl (-> STRUCT,
        // rank 2), so a full 200-TU `kg index` and the 1-TU re-index
        // `kg update` runs on top of it disagreed about node.type for it --
        // caught by `kg dbdiff` on the cucs corpus. First real kind wins is
        // stable across both.
        static const char* kUpdateNodeTypeSql =
            "UPDATE node SET type = ?1 "
            "WHERE id = ?2 AND "
            "((CASE type WHEN ?3 THEN 0 WHEN ?4 THEN 1 ELSE 2 END) < ?5 OR type = ?3)";
        static const char* kUpsertSymbolSql =
            "INSERT INTO symbol(id, definition_kind) VALUES(?1, ?2) "
            "ON CONFLICT(id) DO UPDATE SET definition_kind = max(definition_kind, excluded.definition_kind)";

        for (const auto& [nodeId, kd] : m_pendingKindDef)
        {
            // No "newRank > 0" skip here: even a pending Unknown kind
            // (rank 0) must still be allowed to upgrade a virgin UNKNOWN
            // node (rank 0) to TYPE, matching the old unconditional
            // recordSymbolKind() behavior -- the WHERE clause's own
            // rank(stored) <= newRank guard is what keeps a real kind from
            // ever being downgraded back to TYPE/UNKNOWN.
            {
                const int newNodeKindInt = sourcetrail::nodeKindToInt(sourcetrail::symbolKindToNodeKind(translateSymbolKind(kd.kind)));
                const int newRank = kg::model::kindRank(kd.kind);
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

            if (kd.definition != kg::model::DefinitionKind::None)
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

        static const char* kInsertSymbolDefSql = "INSERT OR IGNORE INTO kg_symbol_def(node_id, file_node_id) VALUES(?1, ?2)";
        for (const auto& [nodeId, fileId] : m_pendingSymbolDef)
        {
            db.execBound(
                kInsertSymbolDefSql,
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_int(stmt, 1, nodeId);
                    sqlite3_bind_int(stmt, 2, fileId);
                });
        }

        static const char* kIndexStatements[] = {
            "CREATE INDEX IF NOT EXISTS kg_edge_source_idx ON edge(source_node_id);",
            "CREATE INDEX IF NOT EXISTS kg_edge_target_idx ON edge(target_node_id);",
            "CREATE INDEX IF NOT EXISTS kg_edge_type_idx ON edge(type);",
            "CREATE INDEX IF NOT EXISTS kg_node_type_idx ON node(type);",
            "CREATE INDEX IF NOT EXISTS kg_srcloc_file_idx ON source_location(file_node_id);",
            "CREATE INDEX IF NOT EXISTS kg_occurrence_elem_idx ON occurrence(element_id);",
            "CREATE INDEX IF NOT EXISTS kg_occurrence_loc_idx ON occurrence(source_location_id);",
            "CREATE INDEX IF NOT EXISTS kg_file_path_idx ON file(path);",
            "CREATE INDEX IF NOT EXISTS kg_symbol_defkind_idx ON symbol(definition_kind);",
        };
        for (const char* stmt : kIndexStatements)
        {
            db.exec(stmt);
        }

        db.commit();

        // ANALYZE/VACUUM are best-effort maintenance, not correctness --
        // a fully-written, already-committed database must not be treated
        // as a failed `kg index` run just because one of these throws (e.g.
        // VACUUM needing free disk space it doesn't have). Downgrade each to
        // an independent stderr warning instead, restoring the previous
        // ignore-result behavior.
        try
        {
            db.exec("ANALYZE;");
        }
        catch (const std::exception& e)
        {
            std::fprintf(stderr, "kg index: warning: ANALYZE failed: %s\n", e.what());
        }
        if (!m_noVacuum && !m_nonIndexedFileIds.empty())
        {
            try
            {
                db.exec("VACUUM;");
            }
            catch (const std::exception& e)
            {
                std::fprintf(stderr, "kg index: warning: VACUUM failed: %s\n", e.what());
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

    summary.symbols = m_summary.symbols;
    summary.references = m_summary.references;
    summary.files = m_summary.files;
    summary.errors = m_summary.errors;
    summary.nameCollisions = m_summary.nameCollisions;
    return true;
}

}  // namespace kg::index
