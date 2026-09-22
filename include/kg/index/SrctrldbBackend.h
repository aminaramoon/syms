#pragma once

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

    // path -> real sqlite node id (shared by FileFact rows and
    // SymbolKind::File "file symbols" used as INCLUDE/MACRO_USAGE edge
    // endpoints -- both ultimately resolve to the SAME node, see
    // SrctrldbBackend.cpp's file-symbol handling).
    std::unordered_map<std::string, int> m_fileNodeIdByPath;
    std::unordered_set<int> m_nonIndexedFileIds;  // for finish()'s post-write UPDATE/DELETE

    // Set when this pass materialized at least one reference to the
    // vendored writer's shared "unsolved symbol" node. That node is created
    // INSIDE the writer (recordReferenceToUnsolvedSymhol()), so it never
    // passes through m_byKey and would otherwise be missing from
    // kg_touched -- see finish().
    bool m_sawUnsolvedReference = false;

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
