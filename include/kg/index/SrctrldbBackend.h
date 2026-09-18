#pragma once

#include <memory>
#include <string>
#include <unordered_map>
#include <unordered_set>

#include "kg/model/IndexBackend.h"

// Forward-declared rather than #included here so headers that only need
// IndexBackend-shaped access to SrctrldbBackend (e.g. IndexDriver.h) don't
// have to pull in the vendored writer's headers transitively.
namespace sourcetrail
{
class SourcetrailDBWriter;
struct NameHierarchy;
}  // namespace sourcetrail

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

    bool begin(const std::string& dbPath, std::string* err) override;
    bool consume(const kg::model::TuFactBatch& batch, std::string* err) override;
    bool finish(kg::model::IndexSummary& summary, std::string* err) override;

private:
    int ensureFileNode(const kg::model::FileFact& fact);
    int ensureFileNodeByPath(const std::string& path, const std::string& language, bool indexed);
    int ensureSymbolNode(const kg::model::SymbolFact& fact);
    int ensureLocalSymbolNode(const std::string& name);
    int ensureReference(int ctxReal, int targetReal, kg::model::ReferenceKind kind);
    void bufferKindDef(int nodeId, kg::model::SymbolKind kind, kg::model::DefinitionKind definition);

    std::unique_ptr<sourcetrail::SourcetrailDBWriter> m_writer;
    std::string m_dbPath;
    bool m_noVacuum = false;
    bool m_verbose = false;

    // path -> real sqlite node id (shared by FileFact rows and
    // SymbolKind::File "file symbols" used as INCLUDE/MACRO_USAGE edge
    // endpoints -- both ultimately resolve to the SAME node, see
    // SrctrldbBackend.cpp's file-symbol handling).
    std::unordered_map<std::string, int> m_fileNodeIdByPath;
    std::unordered_set<int> m_nonIndexedFileIds;  // for finish()'s post-write UPDATE/DELETE

    std::unordered_map<std::string, int> m_byKey;           // SymbolKey -> real node id
    std::unordered_map<std::string, std::string> m_byName;  // serialized_name -> SymbolKey (first owner)

    struct BufferedKindDef
    {
        kg::model::SymbolKind kind = kg::model::SymbolKind::Unknown;
        kg::model::DefinitionKind definition = kg::model::DefinitionKind::None;
    };
    std::unordered_map<int, BufferedKindDef> m_pendingKindDef;

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
