#include "kg/notes/BodyHash.h"

#include <sqlite3.h>

#include <algorithm>
#include <cctype>
#include <vector>

#include "kg/db/Enums.h"
#include "kg/db/Meta.h"
#include "kg/index/ClangUtil.h"

namespace kg::notes
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? std::string(reinterpret_cast<const char*>(text)) : std::string();
}

// Collapses runs of whitespace (including newlines) to a single space and
// trims leading/trailing whitespace -- same normalization Markdown.cpp's
// normalizeSignature() applies to a signature, applied here to a body span.
std::string normalizeWhitespace(const std::string& s)
{
    std::string out;
    bool prevSpace = false;
    for (char c : s)
    {
        if (std::isspace(static_cast<unsigned char>(c)))
        {
            if (!out.empty() && !prevSpace)
            {
                out.push_back(' ');
                prevSpace = true;
            }
        }
        else
        {
            out.push_back(c);
            prevSpace = false;
        }
    }
    while (!out.empty() && out.back() == ' ')
    {
        out.pop_back();
    }
    return out;
}

// Full 16 lowercase hex digits (64 bits) of `v` -- ClangUtil.h's hex8() only
// emits the low 32 bits, which isn't enough entropy for a stable, collision-
// resistant body-hash anchor written into version-controlled markdown.
std::string hex16(uint64_t v)
{
    static const char* digits = "0123456789abcdef";
    std::string out(16, '0');
    for (int i = 15; i >= 0; --i)
    {
        out[static_cast<std::size_t>(i)] = digits[v & 0xFu];
        v >>= 4;
    }
    return out;
}

struct Occ
{
    int64_t fileId = 0;
    std::string filePath;
    int startLine = 0;
    int startColumn = 0;
    int endLine = 0;
    int endColumn = 0;
};

std::string filePathFor(kg::db::Database& db, int64_t fileId)
{
    std::string path;
    db.run(
        "SELECT path FROM file WHERE id = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, fileId); },
        [&](sqlite3_stmt* stmt) { path = columnText(stmt, 0); });
    return path;
}

}  // namespace

std::string bodyHashForNode(kg::db::Database& db, int64_t nodeId)
{
    std::vector<Occ> scopeOccs;
    std::vector<Occ> tokenOccs;

    db.run(
        "SELECT l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type "
        "FROM occurrence o JOIN source_location l ON l.id = o.source_location_id "
        "WHERE o.element_id = ? AND l.type IN (0,1)",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, nodeId); },
        [&](sqlite3_stmt* stmt) {
            Occ occ;
            occ.fileId = sqlite3_column_int64(stmt, 0);
            occ.startLine = sqlite3_column_int(stmt, 1);
            occ.startColumn = sqlite3_column_int(stmt, 2);
            occ.endLine = sqlite3_column_int(stmt, 3);
            occ.endColumn = sqlite3_column_int(stmt, 4);
            const int kind = sqlite3_column_int(stmt, 5);
            occ.filePath = filePathFor(db, occ.fileId);
            if (kind == kg::db::LocationKind::SCOPE)
            {
                scopeOccs.push_back(std::move(occ));
            }
            else
            {
                tokenOccs.push_back(std::move(occ));
            }
        });

    const Occ* chosen = nullptr;
    if (!scopeOccs.empty())
    {
        // Largest line span; ties -> lexicographically smallest file path,
        // then smallest start_line.
        for (const Occ& o : scopeOccs)
        {
            if (chosen == nullptr)
            {
                chosen = &o;
                continue;
            }
            const int curSpan = chosen->endLine - chosen->startLine;
            const int span = o.endLine - o.startLine;
            if (span > curSpan)
            {
                chosen = &o;
            }
            else if (span == curSpan)
            {
                if (o.filePath < chosen->filePath || (o.filePath == chosen->filePath && o.startLine < chosen->startLine))
                {
                    chosen = &o;
                }
            }
        }
    }
    else if (!tokenOccs.empty())
    {
        chosen = &tokenOccs.front();
        for (const Occ& o : tokenOccs)
        {
            if (o.filePath < chosen->filePath || (o.filePath == chosen->filePath && o.startLine < chosen->startLine))
            {
                chosen = &o;
            }
        }
    }

    if (chosen == nullptr)
    {
        return "";
    }

    std::string content;
    bool haveContent = false;
    db.run(
        "SELECT content FROM filecontent WHERE id = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, chosen->fileId); },
        [&](sqlite3_stmt* stmt) {
            content = columnText(stmt, 0);
            haveContent = true;
        });
    if (!haveContent)
    {
        return "";
    }

    // Split into 1-based lines (no trailing newline kept).
    std::vector<std::string> lines;
    {
        std::string cur;
        for (char c : content)
        {
            if (c == '\n')
            {
                if (!cur.empty() && cur.back() == '\r')
                {
                    cur.pop_back();
                }
                lines.push_back(std::move(cur));
                cur.clear();
            }
            else
            {
                cur.push_back(c);
            }
        }
        lines.push_back(std::move(cur));
    }

    const int startLine = std::max(1, chosen->startLine);
    const int endLine = std::max(startLine, chosen->endLine);
    if (startLine > static_cast<int>(lines.size()))
    {
        return "";
    }

    std::string span;
    for (int ln = startLine; ln <= endLine && ln <= static_cast<int>(lines.size()); ++ln)
    {
        std::string lineText = lines[static_cast<std::size_t>(ln - 1)];
        // Clip the trailing edge first (endColumn, only meaningful on the
        // last line), then the leading edge (startColumn, only meaningful
        // on the first line) -- clamped defensively since column offsets
        // are best-effort and must never throw on an out-of-range value.
        if (ln == endLine && chosen->endColumn > 0 && chosen->endColumn - 1 <= static_cast<int>(lineText.size()))
        {
            lineText = lineText.substr(0, static_cast<std::size_t>(chosen->endColumn - 1));
        }
        if (ln == startLine && chosen->startColumn > 1 && chosen->startColumn - 1 <= static_cast<int>(lineText.size()))
        {
            lineText = lineText.substr(static_cast<std::size_t>(chosen->startColumn - 1));
        }
        if (!span.empty())
        {
            span += "\n";
        }
        span += lineText;
    }

    const std::string normalized = normalizeWhitespace(span);
    return hex16(kg::index::clangutil::fnv1a64(normalized));
}

std::string currentCommit(const kg::db::Database& db)
{
    return kg::db::getMeta(db.handle(), "kg_repo_commit").value_or("");
}

}  // namespace kg::notes
