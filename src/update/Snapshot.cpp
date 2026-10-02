#include "kg/update/Snapshot.h"

#include <sqlite3.h>

#include <unordered_map>
#include <unordered_set>

#include "kg/db/Database.h"

namespace kg::update
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? reinterpret_cast<const char*>(text) : "";
}

std::string join(std::initializer_list<std::string> parts)
{
    std::string result;
    bool first = true;
    for (const std::string& part : parts)
    {
        if (!first)
        {
            result += '|';
        }
        first = false;
        result += part;
    }
    return result;
}

}  // namespace

Snapshot takeSnapshot(const std::string& dbPath, const SnapshotOptions& options)
{
    kg::db::Database db(dbPath);
    Snapshot snap;

    // --- shared headers (SnapshotOptions::ignoreSharedHeaderLocations) ---
    // A file that two or more distinct files #include. A header with a
    // single includer cannot carry the divergence this option exists for:
    // if its one includer is cleared, Invalidator's header-pruning step
    // (computeClearSet() step 2) clears the header along with it.
    std::unordered_set<int64_t> sharedHeaderFileIds;
    if (options.ignoreSharedHeaderLocations)
    {
        db.run(
            "SELECT target_node_id FROM edge WHERE type = 256 "
            "GROUP BY target_node_id HAVING COUNT(DISTINCT source_node_id) >= 2",
            [&](sqlite3_stmt* stmt) { sharedHeaderFileIds.insert(sqlite3_column_int64(stmt, 0)); });
    }

    // --- nodes: id -> "serialized_name" (also the elementKey map below) --
    std::unordered_map<int64_t, std::string> nodeSerializedName;
    db.run("SELECT id, type, serialized_name FROM node", [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        const std::string type = std::to_string(sqlite3_column_int(stmt, 1));
        const std::string name = columnText(stmt, 2);
        nodeSerializedName.emplace(id, name);
        snap.nodes[join({name, type})]++;
    });

    // --- edges: id -> "source|target|type" (source/target serialized names)
    std::unordered_map<int64_t, std::string> edgeElementKey;
    db.run("SELECT id, type, source_node_id, target_node_id FROM edge", [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        const std::string type = std::to_string(sqlite3_column_int(stmt, 1));
        const int64_t sourceId = sqlite3_column_int64(stmt, 2);
        const int64_t targetId = sqlite3_column_int64(stmt, 3);
        const auto srcIt = nodeSerializedName.find(sourceId);
        const auto tgtIt = nodeSerializedName.find(targetId);
        const std::string source = srcIt != nodeSerializedName.end() ? srcIt->second : "";
        const std::string target = tgtIt != nodeSerializedName.end() ? tgtIt->second : "";
        const std::string key = join({source, target, type});
        edgeElementKey.emplace(id, key);
        snap.edges[key]++;
    });

    // --- local_symbol: id -> name -------------------------------------------
    std::unordered_map<int64_t, std::string> localSymbolName;
    db.run("SELECT id, name FROM local_symbol", [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        const std::string name = columnText(stmt, 1);
        localSymbolName.emplace(id, name);
        snap.localSymbols[name]++;
    });

    // --- error: id -> message (fatal folded into the snapshot key below) ---
    std::unordered_map<int64_t, std::string> errorMessage;
    std::unordered_map<int64_t, std::string> errorFatal;
    db.run("SELECT id, message, fatal FROM error", [&](sqlite3_stmt* stmt) {
        const int64_t id = sqlite3_column_int64(stmt, 0);
        errorMessage.emplace(id, columnText(stmt, 1));
        errorFatal.emplace(id, std::to_string(sqlite3_column_int(stmt, 2)));
    });

    // --- symbols: (serialized_name, definition_kind) ------------------------
    db.run("SELECT symbol.id, symbol.definition_kind, node.serialized_name FROM symbol JOIN node ON node.id = symbol.id", [&](sqlite3_stmt* stmt) {
        const std::string definitionKind = std::to_string(sqlite3_column_int(stmt, 1));
        const std::string name = columnText(stmt, 2);
        snap.symbols[join({name, definitionKind})]++;
    });

    // --- files: (path, indexed, complete, line_count) ------------------------
    db.run("SELECT path, indexed, complete, line_count FROM file", [&](sqlite3_stmt* stmt) {
        const std::string path = columnText(stmt, 0);
        const std::string indexed = std::to_string(sqlite3_column_int(stmt, 1));
        const std::string complete = std::to_string(sqlite3_column_int(stmt, 2));
        const std::string lineCount = std::to_string(sqlite3_column_int(stmt, 3));
        snap.files[join({path, indexed, complete, lineCount})]++;
    });

    // --- element_id -> first-seen file path, via occurrence/source_location
    // (used both for `errors` below and for `locations`' per-row file path).
    std::unordered_map<int64_t, std::string> elementFirstFilePath;
    db.run(
        "SELECT o.element_id, f.path FROM occurrence o "
        "JOIN source_location sl ON sl.id = o.source_location_id "
        "JOIN file f ON f.id = sl.file_node_id",
        [&](sqlite3_stmt* stmt) {
            const int64_t elementId = sqlite3_column_int64(stmt, 0);
            if (elementFirstFilePath.find(elementId) == elementFirstFilePath.end())
            {
                elementFirstFilePath.emplace(elementId, columnText(stmt, 1));
            }
        });

    // --- errors: (message, fatal, filePathOrDash) ------------------------
    for (const auto& [id, message] : errorMessage)
    {
        const auto fatalIt = errorFatal.find(id);
        const std::string fatal = fatalIt != errorFatal.end() ? fatalIt->second : "0";
        const auto pathIt = elementFirstFilePath.find(id);
        const std::string path = pathIt != elementFirstFilePath.end() ? pathIt->second : "-";
        snap.errors[join({message, fatal, path})]++;
    }

    // --- locations: (file.path, start_line, start_col, end_line, end_col,
    // type, elementKey) -- elementKey depends on which table `element_id`
    // actually belongs to (node/edge/local_symbol/error are the only
    // tables with a FOREIGN KEY(id) REFERENCES element(id) that also show
    // up in `occurrence`).
    db.run(
        "SELECT o.element_id, f.path, sl.start_line, sl.start_column, sl.end_line, sl.end_column, sl.type, f.id "
        "FROM occurrence o "
        "JOIN source_location sl ON sl.id = o.source_location_id "
        "JOIN file f ON f.id = sl.file_node_id",
        [&](sqlite3_stmt* stmt) {
            const int64_t elementId = sqlite3_column_int64(stmt, 0);
            if (!sharedHeaderFileIds.empty() && sharedHeaderFileIds.count(sqlite3_column_int64(stmt, 7)) != 0)
            {
                return;
            }
            const std::string path = columnText(stmt, 1);
            const std::string startLine = std::to_string(sqlite3_column_int(stmt, 2));
            const std::string startCol = std::to_string(sqlite3_column_int(stmt, 3));
            const std::string endLine = std::to_string(sqlite3_column_int(stmt, 4));
            const std::string endCol = std::to_string(sqlite3_column_int(stmt, 5));
            const std::string type = std::to_string(sqlite3_column_int(stmt, 6));

            std::string elementKey;
            if (const auto it = nodeSerializedName.find(elementId); it != nodeSerializedName.end())
            {
                elementKey = "n:" + it->second;
            }
            else if (const auto eit = edgeElementKey.find(elementId); eit != edgeElementKey.end())
            {
                elementKey = "e:" + eit->second;
            }
            else if (const auto lit = localSymbolName.find(elementId); lit != localSymbolName.end())
            {
                elementKey = "l:" + lit->second;
            }
            else if (const auto xit = errorMessage.find(elementId); xit != errorMessage.end())
            {
                elementKey = "x:" + xit->second;
            }
            else
            {
                return;  // unknown element kind -- shouldn't happen, skip
            }

            snap.locations[join({path, startLine, startCol, endLine, endCol, type, elementKey})]++;
        });

    // --- kg_symbol_def: (serialized_name, file.path) -- skip if absent ----
    bool hasSymbolDef = false;
    db.run(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'kg_symbol_def'",
        [&](sqlite3_stmt*) { hasSymbolDef = true; });
    if (hasSymbolDef)
    {
        db.run(
            "SELECT n.serialized_name, f.path FROM kg_symbol_def sd "
            "JOIN node n ON n.id = sd.node_id "
            "JOIN file f ON f.id = sd.file_node_id",
            [&](sqlite3_stmt* stmt) { snap.symbolDefs[join({columnText(stmt, 0), columnText(stmt, 1)})]++; });
    }

    // --- kg_note / kg_note_problem: skip if absent (Phase 4) ----------------
    bool hasNote = false;
    db.run("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'kg_note'", [&](sqlite3_stmt*) { hasNote = true; });
    if (hasNote)
    {
        db.run(
            "SELECT serialized_name, notes_path, brief, what, how, filled, stale, note_hash FROM kg_note",
            [&](sqlite3_stmt* stmt) {
                const std::string serializedName = columnText(stmt, 0);
                const std::string notesPath = columnText(stmt, 1);
                const std::string brief = columnText(stmt, 2);
                const std::string what = columnText(stmt, 3);
                const std::string how = columnText(stmt, 4);
                const std::string filled = std::to_string(sqlite3_column_int(stmt, 5));
                const std::string stale = std::to_string(sqlite3_column_int(stmt, 6));
                const std::string noteHash = columnText(stmt, 7);
                snap.notes[join({serializedName, notesPath, brief, what, how, filled, stale, noteHash})]++;
            });
    }
    bool hasNoteProblem = false;
    db.run("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'kg_note_problem'", [&](sqlite3_stmt*) { hasNoteProblem = true; });
    if (hasNoteProblem)
    {
        db.run("SELECT notes_path, line, kind FROM kg_note_problem", [&](sqlite3_stmt* stmt) {
            const std::string notesPath = columnText(stmt, 0);
            const std::string line = std::to_string(sqlite3_column_int(stmt, 1));
            const std::string kind = columnText(stmt, 2);
            snap.noteProblems[join({notesPath, line, kind})]++;
        });
    }

    // --- kg_tu: (path, args_hash) -- skip if absent -------------------------
    bool hasTu = false;
    db.run("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'kg_tu'", [&](sqlite3_stmt*) { hasTu = true; });
    if (hasTu)
    {
        db.run("SELECT path, args_hash FROM kg_tu", [&](sqlite3_stmt* stmt) {
            snap.tus[join({columnText(stmt, 0), columnText(stmt, 1)})]++;
        });
    }

    return snap;
}

namespace
{

// Diffs one category's (key -> count) maps in a SINGLE pass over the union
// of keys (not one pass per side -- a two-pass "diff A against B, then diff
// B against A" would double-report every count mismatch, once from each
// direction). A key missing from one side goes to that side's list alone
// (category|key); a key present on both sides with a mismatched count goes
// to BOTH lists as "category|key (x<countHere> vs x<countThere>)".
void diffCategory(
    const std::map<std::string, int>& a,
    const std::map<std::string, int>& b,
    const std::string& category,
    std::vector<std::string>& onlyInA,
    std::vector<std::string>& onlyInB)
{
    for (const auto& [key, countA] : a)
    {
        const auto it = b.find(key);
        if (it == b.end())
        {
            onlyInA.push_back(category + "|" + key);
        }
        else if (it->second != countA)
        {
            onlyInA.push_back(category + "|" + key + " (x" + std::to_string(countA) + " vs x" + std::to_string(it->second) + ")");
            onlyInB.push_back(category + "|" + key + " (x" + std::to_string(it->second) + " vs x" + std::to_string(countA) + ")");
        }
    }
    for (const auto& [key, countB] : b)
    {
        if (a.find(key) == a.end())
        {
            onlyInB.push_back(category + "|" + key);
        }
    }
}

}  // namespace

SnapshotDiff diffSnapshots(const Snapshot& a, const Snapshot& b, const std::vector<std::string>& only)
{
    SnapshotDiff diff;
    struct Category
    {
        const std::map<std::string, int> Snapshot::*field;
        const char* name;
    };
    static const Category kCategories[] = {
        {&Snapshot::nodes, "node"},
        {&Snapshot::edges, "edge"},
        {&Snapshot::locations, "location"},
        {&Snapshot::symbols, "symbol"},
        {&Snapshot::files, "file"},
        {&Snapshot::errors, "error"},
        {&Snapshot::localSymbols, "local_symbol"},
        {&Snapshot::symbolDefs, "symbol_def"},
        {&Snapshot::tus, "tu"},
        {&Snapshot::notes, "note"},
        {&Snapshot::noteProblems, "note_problem"},
    };
    const std::unordered_set<std::string> onlySet(only.begin(), only.end());
    for (const Category& cat : kCategories)
    {
        if (!onlySet.empty() && onlySet.count(cat.name) == 0)
        {
            continue;
        }
        diffCategory(a.*cat.field, b.*cat.field, cat.name, diff.onlyInA, diff.onlyInB);
    }
    return diff;
}

}  // namespace kg::update
