#include "kg/notes/Scaffold.h"

#include <sqlite3.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <unordered_map>
#include <unordered_set>

#include "kg/common/Glob.h"
#include "kg/db/Enums.h"
#include "kg/db/Meta.h"
#include "kg/db/NameDecode.h"
#include "kg/notes/BodyHash.h"
#include "kg/notes/Markdown.h"
#include "kg/notes/Sync.h"

namespace kg::notes
{

namespace
{

// Same relative-path derivation runNotesSymbols() (Cli.cpp) used inline
// before this Phase 3 extraction: relative to `repoRoot` when it's known and
// the location actually lives under it, else the raw definition path
// verbatim.
std::string relativeToRepo(const std::string& filePath, const std::string& repoRoot)
{
    if (repoRoot.empty())
    {
        return filePath;
    }
    std::error_code ec;
    std::filesystem::path rel = std::filesystem::relative(filePath, repoRoot, ec);
    if (ec || rel.empty())
    {
        return filePath;
    }
    return rel.generic_string();
}

}  // namespace

SymbolRow rowForNode(kg::db::Database& db, kg::query::QueryEngine& engine, int64_t nodeId, const std::string& repoRoot, const std::string& commit)
{
    SymbolRow row;

    std::string serializedName;
    db.run(
        "SELECT serialized_name FROM node WHERE id = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, nodeId); },
        [&](sqlite3_stmt* stmt) {
            const unsigned char* text = sqlite3_column_text(stmt, 0);
            serializedName = text != nullptr ? reinterpret_cast<const char*>(text) : std::string();
        });
    if (serializedName.empty())
    {
        return row;
    }

    const kg::query::DefinitionResult def = engine.definition({nodeId});
    if (def.locations.empty())
    {
        return row;
    }
    const kg::query::FileLocation& loc = def.locations.front();

    row.sourcePath = relativeToRepo(loc.filePath, repoRoot);
    row.definitionLine = loc.startLine;
    row.qualifiedName = kg::db::decodeSerializedName(serializedName).namePath();
    row.signature = kg::db::decodeSerializedName(serializedName).signature();
    row.bodyHash = bodyHashForNode(db, nodeId);
    row.commit = commit;
    row.nodeId = nodeId;
    row.serializedName = serializedName;
    return row;
}

std::vector<SymbolRow> listSymbols(kg::db::Database& db, int kindMask, const std::vector<std::string>& fileGlobs)
{
    kg::query::QueryEngine engine(db);

    const std::string repoRoot = kg::db::getMeta(db.handle(), "kg_repo_root").value_or("");
    const std::string commit = currentCommit(db);

    const int mask = kindMask != 0 ? kindMask : (kg::db::NodeKind::FUNCTION | kg::db::NodeKind::METHOD);

    std::vector<SymbolRow> rows;
    db.run(
        "SELECT n.id FROM node n JOIN symbol s ON s.id = n.id WHERE (n.type & ?) != 0",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int(stmt, 1, mask); },
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            SymbolRow row = rowForNode(db, engine, id, repoRoot, commit);
            if (row.nodeId != 0)
            {
                rows.push_back(std::move(row));
            }
        });

    if (!fileGlobs.empty())
    {
        std::vector<SymbolRow> filtered;
        for (SymbolRow& r : rows)
        {
            for (const std::string& glob : fileGlobs)
            {
                if (kg::common::glob::match(glob, r.sourcePath))
                {
                    filtered.push_back(std::move(r));
                    break;
                }
            }
        }
        rows = std::move(filtered);
    }

    std::sort(rows.begin(), rows.end(), [](const SymbolRow& a, const SymbolRow& b) {
        if (a.sourcePath != b.sourcePath) return a.sourcePath < b.sourcePath;
        if (a.definitionLine != b.definitionLine) return a.definitionLine < b.definitionLine;
        return a.qualifiedName < b.qualifiedName;
    });
    return rows;
}

ScaffoldResult scaffoldNotes(kg::db::Database& db, const std::vector<SymbolRow>& rowsIn, const ScaffoldOptions& options)
{
    ScaffoldResult result;

    std::vector<SymbolRow> rows = rowsIn;
    std::stable_sort(rows.begin(), rows.end(), [](const SymbolRow& a, const SymbolRow& b) {
        if (a.sourcePath != b.sourcePath) return a.sourcePath < b.sourcePath;
        if (a.definitionLine != b.definitionLine) return a.definitionLine < b.definitionLine;
        return a.qualifiedName < b.qualifiedName;
    });

    std::vector<std::string> order;
    std::unordered_map<std::string, std::vector<SymbolRow>> bySource;
    for (const SymbolRow& r : rows)
    {
        if (bySource.find(r.sourcePath) == bySource.end())
        {
            order.push_back(r.sourcePath);
        }
        bySource[r.sourcePath].push_back(r);
    }

    const std::filesystem::path notesRoot = std::filesystem::absolute(options.notesDir);

    for (const std::string& sourcePath : order)
    {
        const std::vector<SymbolRow>& groupRows = bySource[sourcePath];
        const std::string relPath = sourcePath + ".md";
        const std::filesystem::path full = notesRoot / relPath;

        ScaffoldFileResult fr;
        fr.path = relPath;

        const bool fileExists = std::filesystem::exists(full);
        std::string existingContent;
        std::unordered_set<int64_t> existingNodeIds;
        if (fileExists)
        {
            std::ifstream in(full, std::ios::binary);
            existingContent.assign((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
            const NotesFile parsed = parseNotesFile(existingContent, relPath, false);
            for (const NoteBlock& b : parsed.blocks)
            {
                const MatchOutcome mo = matchBlock(db, b);
                if (mo.kind == MatchKind::Matched)
                {
                    existingNodeIds.insert(mo.nodeId);
                }
            }
        }
        else
        {
            fr.fileCreated = true;
        }

        std::vector<std::string> newBlocks;
        for (const SymbolRow& row : groupRows)
        {
            if (existingNodeIds.find(row.nodeId) != existingNodeIds.end())
            {
                ++fr.skippedExisting;
                continue;
            }
            newBlocks.push_back(placeholderBlockMarkdown(row.qualifiedName, row.signature, row.bodyHash, row.commit));
            ++fr.appended;
        }

        const bool willWrite = !fileExists || fr.appended > 0;
        if (!options.dryRun && willWrite)
        {
            std::string outContent;
            if (fileExists)
            {
                outContent = existingContent;
                while (!outContent.empty() && outContent.back() == '\n')
                {
                    outContent.pop_back();
                }
                for (const std::string& nb : newBlocks)
                {
                    outContent += "\n\n" + nb;
                }
                outContent += "\n";
            }
            else
            {
                outContent = "# " + sourcePath + "\n\n";
                for (std::size_t i = 0; i < newBlocks.size(); ++i)
                {
                    if (i)
                    {
                        outContent += "\n\n";
                    }
                    outContent += newBlocks[i];
                }
                outContent += "\n";
            }
            std::filesystem::create_directories(full.parent_path());
            std::ofstream out(full, std::ios::binary | std::ios::trunc);
            out << outContent;
        }

        if (fr.fileCreated)
        {
            ++result.filesCreated;
        }
        if (fr.fileCreated || fr.appended > 0)
        {
            ++result.filesTouched;
        }
        result.totalAppended += fr.appended;
        result.totalSkipped += fr.skippedExisting;
        result.files.push_back(std::move(fr));
    }

    return result;
}

}  // namespace kg::notes
