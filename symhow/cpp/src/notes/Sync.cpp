#include "symhow/notes/Sync.h"

#include <sqlite3.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <unordered_map>
#include <unordered_set>

#include <nlohmann/json.hpp>

#include "symhow/db/Meta.h"
#include "symhow/db/NameDecode.h"
#include "symhow/db/WritableDb.h"
#include "symhow/index/ClangUtil.h"
#include "symhow/notes/BodyHash.h"
#include "symhow/notes/Markdown.h"
#include "symhow/notes/NotesStore.h"
#include "symhow/query/SymbolResolver.h"

namespace symhow::notes
{

namespace
{

// Compares two signature strings for the "parameter-list suffix" fallback:
// the text from the first '(' onward (inclusive), or the whole (already
// normalizeSignature()-normalized) string if it has no '('.
std::string paramSuffix(const std::string& normalizedSig)
{
    const std::string::size_type p = normalizedSig.find('(');
    return p == std::string::npos ? normalizedSig : normalizedSig.substr(p);
}

std::string jsonArray(const std::vector<std::string>& items)
{
    nlohmann::json j = nlohmann::json::array();
    for (const std::string& s : items)
    {
        j.push_back(s);
    }
    return j.dump();
}

}  // namespace

MatchOutcome matchBlock(symhow::db::Database& db, const NoteBlock& block)
{
    const symhow::query::ResolveResult rr = symhow::query::resolveSymbol(db, block.heading, nullptr, nullptr);

    if (rr.candidates.empty())
    {
        MatchOutcome out;
        out.kind = MatchKind::Unmatched;
        return out;
    }

    // Exactly one candidate already, before any signature-based narrowing --
    // matches unconditionally (one candidate is one candidate), but flag a
    // "signature-mismatch" when the block's own `### signature` disagrees
    // with it (see MatchOutcome::signatureMismatch's doc comment).
    if (rr.candidates.size() == 1)
    {
        const symhow::query::ResolvedSymbol& c = rr.candidates.front();
        const symhow::db::DecodedName decoded = symhow::db::decodeSerializedName(c.serializedName);
        MatchOutcome out;
        out.kind = MatchKind::Matched;
        out.nodeId = c.id;
        out.serializedName = c.serializedName;
        out.qualifiedName = decoded.namePath();
        out.signature = decoded.signature();

        if (!block.signature.text.empty())
        {
            const std::string wantFull = normalizeSignature(block.signature.text);
            const std::string wantSuffix = paramSuffix(wantFull);
            const std::string haveFull = normalizeSignature(decoded.signature());
            const std::string haveSuffix = paramSuffix(haveFull);
            if (haveFull != wantFull && haveSuffix != wantSuffix)
            {
                out.signatureMismatch = true;
                out.detail = "expected '" + wantFull + "', found '" + haveFull + "'";
            }
        }
        return out;
    }

    std::vector<symhow::query::ResolvedSymbol> pool = rr.candidates;
    {
        const std::string wantFull = normalizeSignature(block.signature.text);
        const std::string wantSuffix = paramSuffix(wantFull);

        std::vector<symhow::query::ResolvedSymbol> filtered;
        for (const symhow::query::ResolvedSymbol& c : pool)
        {
            const symhow::db::DecodedName decoded = symhow::db::decodeSerializedName(c.serializedName);
            const std::string haveFull = normalizeSignature(decoded.signature());
            const std::string haveSuffix = paramSuffix(haveFull);
            if (!wantFull.empty() && (haveFull == wantFull || haveSuffix == wantSuffix))
            {
                filtered.push_back(c);
            }
        }
        pool = std::move(filtered);
    }

    if (pool.size() == 1)
    {
        const symhow::db::DecodedName decoded = symhow::db::decodeSerializedName(pool.front().serializedName);
        MatchOutcome out;
        out.kind = MatchKind::Matched;
        out.nodeId = pool.front().id;
        out.serializedName = pool.front().serializedName;
        out.qualifiedName = decoded.namePath();
        out.signature = decoded.signature();
        return out;
    }

    // Either the original resolveSymbol() call was itself Ambiguous and
    // signature narrowing didn't get to exactly 1 (0 or >1 left), or (rare)
    // it was already >1 with no narrowing possible.
    MatchOutcome out;
    out.kind = MatchKind::Ambiguous;
    std::string detail = "candidates: ";
    for (std::size_t i = 0; i < rr.candidates.size(); ++i)
    {
        if (i)
        {
            detail += "; ";
        }
        const symhow::db::DecodedName decoded = symhow::db::decodeSerializedName(rr.candidates[i].serializedName);
        detail += "#" + std::to_string(rr.candidates[i].id) + " " + decoded.signature();
    }
    out.detail = detail;
    return out;
}

SyncResult syncNotes(const std::string& dbPath, const std::string& notesDir, bool strict)
{
    SyncResult result;

    const std::filesystem::path notesRoot = std::filesystem::absolute(notesDir);

    std::vector<std::string> mdFiles;
    if (std::filesystem::is_directory(notesRoot))
    {
        for (const std::filesystem::directory_entry& e : std::filesystem::recursive_directory_iterator(notesRoot))
        {
            if (e.is_regular_file() && e.path().extension() == ".md")
            {
                mdFiles.push_back(e.path().string());
            }
        }
    }
    std::sort(mdFiles.begin(), mdFiles.end());
    result.counts.files = mdFiles.size();

    // Parse every file first (no DB handle needed for this part).
    std::vector<NotesFile> parsed;
    parsed.reserve(mdFiles.size());
    for (const std::string& full : mdFiles)
    {
        std::ifstream in(full, std::ios::binary);
        std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        const std::string relPath = std::filesystem::relative(full, notesRoot).generic_string();
        parsed.push_back(parseNotesFile(content, relPath, strict));
    }

    symhow::db::WritableDb wdb(dbPath);
    wdb.exec(kCreateNoteTableSql);
    wdb.exec(kCreateNoteFileTableSql);
    wdb.exec(kCreateNoteProblemTableSql);

    wdb.begin();
    try
    {
        wdb.exec("DELETE FROM symhow_note;");
        wdb.exec("DELETE FROM symhow_note_file;");
        wdb.exec("DELETE FROM symhow_note_problem;");

        // Need a read-only Database handle to run resolveSymbol()/
        // bodyHashForNode() against the same file -- WritableDb's own
        // in-progress transaction is visible to a second connection to the
        // same file only once committed, but SQLite's default journal mode
        // allows a read-only connection opened AFTER the write connection to
        // see uncommitted changes within the same process via the same
        // underlying file only if using WAL; to stay simple and correct
        // regardless of journal mode, all matching happens BEFORE this
        // transaction's DELETEs matter -- resolveSymbol/bodyHashForNode only
        // ever read `node`/`symbol`/`occurrence`/`source_location`/
        // `filecontent`/`file`, tables this sync never modifies, so reading
        // them through a fresh read-only Database opened on the same path
        // is safe and simple.
        symhow::db::Database rdb(dbPath);

        std::unordered_map<int64_t, std::string> matchedNodeToHeadingPath;  // nodeId -> "path:line" of first match

        // Phase 3 "orphaned-file" detection: a notes file whose source path
        // (its own notes_path with the trailing ".md" stripped) no longer
        // exists as a `file` row -- e.g. after `git mv`ing the source file
        // without moving its notes alongside it. Only meaningful when
        // symhow_repo_root is known (nothing to resolve the relative path
        // against otherwise); silently skipped when it isn't.
        const std::string repoRootForOrphanCheck = symhow::db::getMeta(rdb.handle(), "symhow_repo_root").value_or("");

        const std::string syncedAt = []() {
            std::time_t t = std::time(nullptr);
            char buf[32];
            std::strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%SZ", std::gmtime(&t));
            return std::string(buf);
        }();

        for (std::size_t fi = 0; fi < parsed.size(); ++fi)
        {
            const NotesFile& file = parsed[fi];

            if (!repoRootForOrphanCheck.empty() && file.path.size() > 3 && file.path.compare(file.path.size() - 3, 3, ".md") == 0)
            {
                const std::string sourcePath = file.path.substr(0, file.path.size() - 3);
                const std::filesystem::path absSource =
                    std::filesystem::path(repoRootForOrphanCheck) / sourcePath;
                const std::string absSourceStr = absSource.lexically_normal().string();
                bool exists = false;
                rdb.run(
                    "SELECT 1 FROM file WHERE path = ?",
                    [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, absSourceStr.c_str(), -1, SQLITE_TRANSIENT); },
                    [&](sqlite3_stmt*) { exists = true; });
                if (!exists)
                {
                    ++result.counts.orphanedFiles;
                    result.problems.push_back(SyncProblem{
                        file.path,
                        1,
                        "",
                        "orphaned-file",
                        "source file '" + sourcePath + "' no longer exists in the index"});
                }
            }

            for (const Problem& p : file.problems)
            {
                result.problems.push_back(SyncProblem{file.path, p.line, "", p.kind, p.message});
                ++result.counts.lintProblems;
            }

            for (const NoteBlock& b : file.blocks)
            {
                ++result.counts.blocks;

                const MatchOutcome mo = matchBlock(rdb, b);
                if (mo.kind == MatchKind::Unmatched)
                {
                    ++result.counts.unmatched;
                    result.problems.push_back(
                        SyncProblem{file.path, b.headingLine, b.heading, "unmatched", "no symbol matches heading '" + b.heading + "'"});
                    continue;
                }
                if (mo.kind == MatchKind::Ambiguous)
                {
                    ++result.counts.ambiguous;
                    result.problems.push_back(SyncProblem{
                        file.path, b.headingLine, b.heading, "ambiguous", "heading '" + b.heading + "' is ambiguous -- " + mo.detail});
                    continue;
                }

                const auto already = matchedNodeToHeadingPath.find(mo.nodeId);
                if (already != matchedNodeToHeadingPath.end())
                {
                    ++result.counts.duplicateTarget;
                    result.problems.push_back(SyncProblem{
                        file.path,
                        b.headingLine,
                        b.heading,
                        "duplicate-target",
                        "resolves to the same symbol as " + already->second + " -- ignored"});
                    continue;
                }
                matchedNodeToHeadingPath[mo.nodeId] = file.path + ":" + std::to_string(b.headingLine);

                if (mo.signatureMismatch)
                {
                    ++result.counts.signatureMismatch;
                    result.problems.push_back(SyncProblem{
                        file.path,
                        b.headingLine,
                        b.heading,
                        "signature-mismatch",
                        "heading '" + b.heading + "' -- " + mo.detail});
                }

                const std::string currentBodyHash = bodyHashForNode(rdb, mo.nodeId);
                const bool stale = !b.bodyHash.empty() && !currentBodyHash.empty() && b.bodyHash != currentBodyHash;
                const bool filled = b.filled();

                ++result.counts.matched;
                if (filled)
                {
                    ++result.counts.filled;
                }
                if (stale)
                {
                    ++result.counts.stale;
                }
                if (b.bodyHash.empty())
                {
                    ++result.counts.unanchored;
                }

                wdb.execBound(
                    "INSERT INTO symhow_note(serialized_name, qualified_name, notes_path, heading_line, brief, what, how, "
                    "signature_text, pre_json, post_json, bugs_json, suggestions_json, filled, note_hash, note_commit, "
                    "current_body_hash, stale, synced_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    [&](sqlite3_stmt* stmt) {
                        int i = 1;
                        sqlite3_bind_text(stmt, i++, mo.serializedName.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, mo.qualifiedName.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, file.path.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_int(stmt, i++, b.headingLine);
                        sqlite3_bind_text(stmt, i++, b.brief.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, b.what.text.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, b.how.text.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, mo.signature.c_str(), -1, SQLITE_TRANSIENT);
                        const std::string preJ = jsonArray(b.pre.items);
                        const std::string postJ = jsonArray(b.post.items);
                        const std::string bugsJ = jsonArray(b.bugs.items);
                        const std::string sugJ = jsonArray(b.suggestions.items);
                        sqlite3_bind_text(stmt, i++, preJ.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, postJ.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, bugsJ.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, sugJ.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_int(stmt, i++, filled ? 1 : 0);
                        sqlite3_bind_text(stmt, i++, b.bodyHash.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, b.commit.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_text(stmt, i++, currentBodyHash.c_str(), -1, SQLITE_TRANSIENT);
                        sqlite3_bind_int(stmt, i++, stale ? 1 : 0);
                        sqlite3_bind_text(stmt, i++, syncedAt.c_str(), -1, SQLITE_TRANSIENT);
                    });
            }
        }

        for (const std::string& full : mdFiles)
        {
            const std::string relPath = std::filesystem::relative(full, notesRoot).generic_string();
            std::ifstream in(full, std::ios::binary);
            std::string content((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
            const uint64_t contentHash = symhow::index::clangutil::fnv1a64(content);
            char hexbuf[17];
            std::snprintf(hexbuf, sizeof(hexbuf), "%016llx", static_cast<unsigned long long>(contentHash));
            const auto mtime = std::filesystem::last_write_time(full);
            const int64_t mtimeNs = std::chrono::duration_cast<std::chrono::nanoseconds>(mtime.time_since_epoch()).count();

            wdb.execBound(
                "INSERT INTO symhow_note_file(notes_path, mtime_ns, content_hash) VALUES (?,?,?)",
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_text(stmt, 1, relPath.c_str(), -1, SQLITE_TRANSIENT);
                    sqlite3_bind_int64(stmt, 2, mtimeNs);
                    sqlite3_bind_text(stmt, 3, hexbuf, -1, SQLITE_TRANSIENT);
                });
        }

        for (const SyncProblem& p : result.problems)
        {
            wdb.execBound(
                "INSERT INTO symhow_note_problem(notes_path, line, heading, kind, message) VALUES (?,?,?,?,?)",
                [&](sqlite3_stmt* stmt) {
                    sqlite3_bind_text(stmt, 1, p.path.c_str(), -1, SQLITE_TRANSIENT);
                    sqlite3_bind_int(stmt, 2, p.line);
                    sqlite3_bind_text(stmt, 3, p.heading.c_str(), -1, SQLITE_TRANSIENT);
                    sqlite3_bind_text(stmt, 4, p.kind.c_str(), -1, SQLITE_TRANSIENT);
                    sqlite3_bind_text(stmt, 5, p.message.c_str(), -1, SQLITE_TRANSIENT);
                });
        }

        symhow::db::setMeta(wdb, "symhow_notes_dir", notesRoot.string());
        symhow::db::setMeta(wdb, "symhow_notes_synced_at", syncedAt);

        wdb.commit();
    }
    catch (...)
    {
        wdb.rollback();
        throw;
    }

    return result;
}

}  // namespace symhow::notes
