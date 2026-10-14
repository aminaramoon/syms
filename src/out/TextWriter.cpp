#include "symhow/out/TextWriter.h"

#include <iomanip>
#include <sstream>

#include "symhow/db/Enums.h"
#include "symhow/db/NameDecode.h"

namespace symhow::out
{

namespace
{

std::string displayNameOf(const std::string& serializedName)
{
    return symhow::db::decodeSerializedName(serializedName).displayName();
}

}  // namespace

void writeBfsResult(
    std::ostream& out,
    const symhow::query::BfsResult& result,
    const std::unordered_map<int64_t, std::string>& locations,
    bool showEdgeList,
    const std::unordered_map<int64_t, NodeNoteText>* notes,
    bool notesFull)
{
    if (result.nodes.empty())
    {
        out << "(no nodes)\n";
    }
    for (const symhow::query::BfsNode& node : result.nodes)
    {
        const std::string indent(static_cast<std::size_t>(node.depth) * 2, ' ');
        out << indent << "[" << symhow::db::nodeKindToString(node.kind) << "] " << displayNameOf(node.serializedName) << " (#" << node.id << ")";
        const auto it = locations.find(node.id);
        if (it != locations.end() && !it->second.empty())
        {
            out << "  (" << it->second << ")";
        }
        out << "\n";

        if (notes != nullptr)
        {
            const auto nit = notes->find(node.id);
            if (nit != notes->end())
            {
                const std::string noteIndent = indent + "    ";
                const char* tag = nit->second.stale ? " [stale]" : (nit->second.filled ? "" : " [unfilled]");
                if (notesFull && !nit->second.fullMarkdown.empty())
                {
                    out << noteIndent << "note" << tag << ":\n";
                    std::istringstream iss(nit->second.fullMarkdown);
                    std::string line;
                    while (std::getline(iss, line))
                    {
                        out << noteIndent << "  " << line << "\n";
                    }
                }
                else
                {
                    out << noteIndent << "brief: " << nit->second.brief << tag << "\n";
                }
            }
        }
    }

    out << result.edges.size() << " edge(s)" << (result.truncated ? " [truncated: --max-nodes reached]" : "") << "\n";
    if (result.truncated)
    {
        // Phase 5's JSON writer surfaces the same signal as BfsResult::
        // truncated / "truncated" in the envelope; this is the text-mode
        // equivalent, called out explicitly in the Phase 5 spec even though
        // TextWriter.cpp itself is a Phase 4 file.
        out << "... truncated at " << result.nodes.size() << " nodes\n";
    }
    if (showEdgeList)
    {
        for (const symhow::query::BfsEdge& edge : result.edges)
        {
            out << "  #" << edge.source << " --[" << symhow::db::edgeKindToString(edge.kind) << "]--> #" << edge.target << "\n";
        }
    }
}

void writeReferences(std::ostream& out, const symhow::query::ReferencesResult& result)
{
    if (result.hits.empty())
    {
        out << "(no references)\n";
        return;
    }
    for (const symhow::query::ReferenceHit& hit : result.hits)
    {
        out << hit.location.filePath << ":" << hit.location.startLine << ":" << hit.location.startColumn << "  ["
            << symhow::db::locationKindToString(hit.location.locationKind) << "]  ";
        if (hit.hasEdge)
        {
            out << symhow::db::edgeKindToString(hit.edgeKind) << "  #" << hit.contextNodeId << " -> #" << hit.targetNodeId;
        }
        else
        {
            out << "declaration  #" << hit.targetNodeId;
        }
        out << "\n";
    }
}

void writeDefinition(std::ostream& out, const symhow::query::DefinitionResult& result)
{
    if (result.locations.empty())
    {
        out << "(no definition found)\n";
        return;
    }
    for (const symhow::query::FileLocation& loc : result.locations)
    {
        out << loc.filePath << ":" << loc.startLine << ":" << loc.startColumn << "-" << loc.endLine << ":" << loc.endColumn << "  ["
            << symhow::db::locationKindToString(loc.locationKind) << "]\n";
    }
}

void writeSearchHits(std::ostream& out, const std::vector<symhow::query::SearchHit>& hits)
{
    if (hits.empty())
    {
        out << "(no matches)\n";
        return;
    }
    for (const symhow::query::SearchHit& hit : hits)
    {
        out << std::setw(4) << hit.score << "  [" << symhow::db::nodeKindToString(hit.kind) << "]  " << hit.displayName << "  (" << hit.qualifiedName
            << ")" << (hit.indexed ? "" : "  [not indexed]") << "\n";
    }
}

void writeFileResult(std::ostream& out, const symhow::query::FileResult& result)
{
    out << "path: " << result.path << "\n";
    out << "id: " << result.fileId << "\n";
    out << "language: " << result.language << "\n";
    out << "indexed: " << (result.indexed ? "true" : "false") << "\n";
    out << "complete: " << (result.complete ? "true" : "false") << "\n";
    out << "line_count: " << result.lineCount << "\n";

    if (result.hasContents)
    {
        out << "--- contents ---\n" << result.contents << "\n";
    }

    if (!result.occurrences.empty())
    {
        out << "--- occurrences (" << result.occurrences.size() << ") ---\n";
        for (const symhow::query::FileOccurrence& occ : result.occurrences)
        {
            out << occ.location.startLine << ":" << occ.location.startColumn << "  ";
            if (occ.elementKind != 0)
            {
                out << "[" << symhow::db::nodeKindToString(occ.elementKind) << "] " << occ.elementName;
            }
            else
            {
                out << "(non-node element #" << occ.elementId << ")";
            }
            out << "\n";
        }
    }
}

void writeStats(std::ostream& out, const symhow::query::StatsResult& result)
{
    out << "storage_version: " << result.storageVersion << "\n";
    out << "nodes: " << result.nodeCount << "\n";
    out << "edges: " << result.edgeCount << "\n";
    out << "files: " << result.fileCount << "\n";
    out << "symbols: " << result.symbolCount << "\n";
    out << "occurrences: " << result.occurrenceCount << "\n";
    out << "source_locations: " << result.sourceLocationCount << "\n";
    out << "local_symbols: " << result.localSymbolCount << "\n";
    out << "errors: " << result.errorCount << "\n";

    out << "--- nodes by kind ---\n";
    for (const symhow::query::KindCount& kc : result.nodesByKind)
    {
        out << "  " << symhow::db::nodeKindToString(kc.kind) << ": " << kc.count << "\n";
    }
    out << "--- edges by kind ---\n";
    for (const symhow::query::KindCount& kc : result.edgesByKind)
    {
        out << "  " << symhow::db::edgeKindToString(kc.kind) << ": " << kc.count << "\n";
    }

    if (!result.errors.empty())
    {
        out << "--- errors (" << result.errors.size() << " shown) ---\n";
        for (const symhow::query::StatsErrorRow& e : result.errors)
        {
            out << "  #" << e.id << (e.fatal ? " [FATAL] " : " ") << e.translationUnit << ": " << e.message << "\n";
        }
    }
}

void writeCandidateTable(std::ostream& out, const std::vector<CandidateRow>& rows)
{
    for (const CandidateRow& row : rows)
    {
        out << "  #" << row.id << "  [" << row.kind << "]  " << row.name;
        if (!row.location.empty())
        {
            out << "  (" << row.location << ")";
        }
        out << "\n";
    }
}

}  // namespace symhow::out
