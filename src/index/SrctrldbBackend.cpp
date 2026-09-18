#include "kg/index/SrctrldbBackend.h"

#include <cstdio>
#include <sstream>

#include <sqlite3.h>

#include "DefinitionKind.h"
#include "NameHierarchy.h"
#include "ReferenceKind.h"
#include "SourceRange.h"
#include "SourcetrailDBWriter.h"
#include "SymbolKind.h"

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

bool execSql(sqlite3* db, const std::string& sql, std::string* err)
{
    char* errMsg = nullptr;
    const int rc = sqlite3_exec(db, sql.c_str(), nullptr, nullptr, &errMsg);
    if (rc != SQLITE_OK)
    {
        if (err != nullptr)
        {
            *err = "sqlite error running '" + sql + "': " + (errMsg != nullptr ? errMsg : "unknown error");
        }
        if (errMsg != nullptr)
        {
            sqlite3_free(errMsg);
        }
        return false;
    }
    return true;
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
                    m_summary.references++;
                }
                break;
            }
        }
    }

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
        else if (!fileReal.empty())
        {
            sr.fileId = fileReal.front();
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

    for (const auto& [nodeId, kd] : m_pendingKindDef)
    {
        m_writer->recordSymbolKind(nodeId, translateSymbolKind(kd.kind));
        if (kd.definition != kg::model::DefinitionKind::None)
        {
            m_writer->recordSymbolDefinitionKind(nodeId, translateDefinitionKind(kd.definition));
        }
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
    // handle -- never patch the vendored writer to do this instead.
    sqlite3* db = nullptr;
    if (sqlite3_open_v2(m_dbPath.c_str(), &db, SQLITE_OPEN_READWRITE, nullptr) != SQLITE_OK)
    {
        if (err != nullptr)
        {
            *err = "failed to reopen '" + m_dbPath + "' for post-write fixups";
        }
        if (db != nullptr)
        {
            sqlite3_close(db);
        }
        return false;
    }

    bool ok = execSql(db, "BEGIN;", err);

    if (ok && !m_nonIndexedFileIds.empty())
    {
        const std::string idList = joinIds(m_nonIndexedFileIds);
        ok = ok && execSql(db, "UPDATE file SET indexed = 0, complete = 0 WHERE id IN (" + idList + ");", err);
        ok = ok && execSql(db, "DELETE FROM filecontent WHERE id IN (" + idList + ");", err);
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
        ok = ok && execSql(db, stmt, err);
    }

    ok = ok && execSql(db, "COMMIT;", err);

    if (ok)
    {
        execSql(db, "ANALYZE;", nullptr);
        if (!m_noVacuum && !m_nonIndexedFileIds.empty())
        {
            execSql(db, "VACUUM;", nullptr);
        }
    }

    sqlite3_close(db);

    if (!ok)
    {
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
