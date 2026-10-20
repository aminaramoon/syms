#include "symhow/notes/NotesStore.h"

#include <sqlite3.h>

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>

#include <nlohmann/json.hpp>

#include "symhow/db/Meta.h"
#include "symhow/index/ClangUtil.h"
#include "symhow/notes/BodyHash.h"
#include "symhow/notes/Markdown.h"
#include "symhow/notes/Sync.h"

namespace symhow::notes
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? std::string(reinterpret_cast<const char*>(text)) : std::string();
}

std::vector<std::string> jsonArrayToVec(const std::string& s)
{
    std::vector<std::string> out;
    if (s.empty())
    {
        return out;
    }
    try
    {
        const nlohmann::json j = nlohmann::json::parse(s);
        if (j.is_array())
        {
            for (const auto& e : j)
            {
                out.push_back(e.get<std::string>());
            }
        }
    }
    catch (...)
    {
    }
    return out;
}

// Same content-hash formula Sync.cpp's syncNotes() writes into
// symhow_note_file.content_hash -- 16 lowercase hex digits of FNV-1a-64 over the
// raw file bytes.
std::string contentHashOf(const std::string& bytes)
{
    char buf[17];
    std::snprintf(buf, sizeof(buf), "%016llx", static_cast<unsigned long long>(symhow::index::clangutil::fnv1a64(bytes)));
    return std::string(buf);
}

bool tableExists(symhow::db::Database& db, const std::string& name)
{
    bool found = false;
    db.run(
        "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, name.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt*) { found = true; });
    return found;
}

StoredNote noteFromRow(sqlite3_stmt* stmt)
{
    StoredNote n;
    n.serializedName = columnText(stmt, 0);
    n.qualifiedName = columnText(stmt, 1);
    n.notesPath = columnText(stmt, 2);
    n.headingLine = sqlite3_column_int(stmt, 3);
    n.brief = columnText(stmt, 4);
    n.what = columnText(stmt, 5);
    n.how = columnText(stmt, 6);
    n.signatureText = columnText(stmt, 7);
    n.pre = jsonArrayToVec(columnText(stmt, 8));
    n.post = jsonArrayToVec(columnText(stmt, 9));
    n.bugs = jsonArrayToVec(columnText(stmt, 10));
    n.suggestions = jsonArrayToVec(columnText(stmt, 11));
    n.filled = sqlite3_column_int(stmt, 12) != 0;
    n.noteHash = columnText(stmt, 13);
    n.noteCommit = columnText(stmt, 14);
    n.currentBodyHash = columnText(stmt, 15);
    n.stale = sqlite3_column_int(stmt, 16) != 0;
    n.syncedAt = columnText(stmt, 17);
    return n;
}

StoredNote noteFromBlock(symhow::db::Database& db, const std::string& notesPath, const NoteBlock& b, const MatchOutcome& mo)
{
    StoredNote n;
    n.serializedName = mo.serializedName;
    n.qualifiedName = mo.qualifiedName;
    n.notesPath = notesPath;
    n.headingLine = b.headingLine;
    n.brief = b.brief;
    n.what = b.what.text;
    n.how = b.how.text;
    n.signatureText = mo.signature;
    n.pre = b.pre.items;
    n.post = b.post.items;
    n.bugs = b.bugs.items;
    n.suggestions = b.suggestions.items;
    n.filled = b.filled();
    n.noteHash = b.bodyHash;
    n.noteCommit = b.commit;
    n.currentBodyHash = bodyHashForNode(db, mo.nodeId);
    n.stale = !n.noteHash.empty() && !n.currentBodyHash.empty() && n.noteHash != n.currentBodyHash;
    n.syncedAt = "";  // overlay entry, not persisted -- never synced
    return n;
}

}  // namespace

NoteLookup openNotes(symhow::db::Database& db)
{
    NoteLookup lookup;

    if (!tableExists(db, "symhow_note"))
    {
        return lookup;
    }

    db.run(
        "SELECT serialized_name, qualified_name, notes_path, heading_line, brief, what, how, signature_text, "
        "pre_json, post_json, bugs_json, suggestions_json, filled, note_hash, note_commit, current_body_hash, "
        "stale, synced_at FROM symhow_note",
        [&](sqlite3_stmt* stmt) {
            StoredNote n = noteFromRow(stmt);
            lookup.bySerializedName[n.serializedName] = std::move(n);
        });

    const std::optional<std::string> notesDirOpt = symhow::db::getMeta(db.handle(), "symhow_notes_dir");
    if (!notesDirOpt.has_value() || notesDirOpt->empty())
    {
        return lookup;
    }
    const std::filesystem::path notesRoot = *notesDirOpt;
    if (!std::filesystem::is_directory(notesRoot))
    {
        return lookup;
    }

    // notes_path -> content_hash as of the last sync.
    std::unordered_map<std::string, std::string> lastSyncedHash;
    if (tableExists(db, "symhow_note_file"))
    {
        db.run("SELECT notes_path, content_hash FROM symhow_note_file", [&](sqlite3_stmt* stmt) {
            lastSyncedHash[columnText(stmt, 0)] = columnText(stmt, 1);
        });
    }

    std::vector<std::string> mdFiles;
    for (const std::filesystem::directory_entry& e : std::filesystem::recursive_directory_iterator(notesRoot))
    {
        if (e.is_regular_file() && e.path().extension() == ".md")
        {
            mdFiles.push_back(e.path().string());
        }
    }
    std::sort(mdFiles.begin(), mdFiles.end());

    for (const std::string& full : mdFiles)
    {
        const std::string relPath = std::filesystem::relative(full, notesRoot).generic_string();
        std::ifstream in(full, std::ios::binary);
        std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        const std::string hash = contentHashOf(content);

        const auto it = lastSyncedHash.find(relPath);
        const bool changed = (it == lastSyncedHash.end()) || (it->second != hash);
        if (!changed)
        {
            continue;
        }

        lookup.overlayFiles.push_back(relPath);

        // Drop every cached entry that used to belong to this file (it may
        // have lost blocks entirely) before re-adding whatever the current
        // on-disk content matches to.
        for (auto entryIt = lookup.bySerializedName.begin(); entryIt != lookup.bySerializedName.end();)
        {
            if (entryIt->second.notesPath == relPath)
            {
                entryIt = lookup.bySerializedName.erase(entryIt);
            }
            else
            {
                ++entryIt;
            }
        }

        const NotesFile parsedFile = parseNotesFile(content, relPath, false);
        for (const NoteBlock& b : parsedFile.blocks)
        {
            const MatchOutcome mo = matchBlock(db, b);
            if (mo.kind != MatchKind::Matched)
            {
                continue;
            }
            lookup.bySerializedName[mo.serializedName] = noteFromBlock(db, relPath, b, mo);
        }
    }

    std::sort(lookup.overlayFiles.begin(), lookup.overlayFiles.end());
    return lookup;
}

}  // namespace symhow::notes
