#include "symhow/query/SymbolResolver.h"

#include <sqlite3.h>

#include <algorithm>
#include <cctype>
#include <charconv>

#include "symhow/db/Enums.h"
#include "symhow/db/NameDecode.h"

namespace symhow::query
{

namespace
{

std::string columnText(sqlite3_stmt* stmt, int col)
{
    const unsigned char* text = sqlite3_column_text(stmt, col);
    return text != nullptr ? reinterpret_cast<const char*>(text) : std::string();
}

std::string lastSegment(const std::string& spec)
{
    const std::string::size_type pos = spec.rfind("::");
    if (pos == std::string::npos)
    {
        return spec;
    }
    return spec.substr(pos + 2);
}

// The cheap LIKE pre-filter key: the leading identifier-shaped run of
// lastSegment(spec) (letters/digits/'_'/'~', for destructor names). A raw
// `node.serialized_name` stores each element's bare `.name` as a literal
// contiguous substring (immediately after a "\tm"/"\tn" delimiter byte), but
// NOT the whole qualifiedName()/signature() string verbatim -- a spec like
// "helper() [tu_a.cpp#317677a3]" (qualifiedName()'s round-trip form, with no
// "::" to split on) has "() [tu_a.cpp#317677a3]" glued directly onto
// "helper" with no delimiter, whereas the on-disk bytes are
// "helper\tsvoid \tp() [tu_a.cpp#317677a3]" -- so a LIKE search for the
// WHOLE spec would never find it. Trimming to just the leading identifier
// avoids that: it's always a literal substring on disk, and remains a valid
// (if slightly wider) pre-filter for plain bare-name specs too. If `spec`
// doesn't start with an identifier character at all (e.g. spec is just
// "()"), this degrades to an empty key, which makes the LIKE pattern
// '%%' -- a full scan, but still correct.
std::string likePrefilterKey(const std::string& spec)
{
    const std::string seg = lastSegment(spec);
    std::string::size_type cut = 0;
    while (cut < seg.size() && (std::isalnum(static_cast<unsigned char>(seg[cut])) != 0 || seg[cut] == '_' || seg[cut] == '~'))
    {
        ++cut;
    }
    return seg.substr(0, cut);
}

void dedupById(std::vector<ResolvedSymbol>& candidates)
{
    std::sort(candidates.begin(), candidates.end(), [](const ResolvedSymbol& a, const ResolvedSymbol& b) { return a.id < b.id; });
    candidates.erase(
        std::unique(candidates.begin(), candidates.end(), [](const ResolvedSymbol& a, const ResolvedSymbol& b) { return a.id == b.id; }),
        candidates.end());
}

ResolveResult finalize(std::vector<ResolvedSymbol> candidates)
{
    ResolveResult result;
    dedupById(candidates);
    if (candidates.empty())
    {
        result.outcome = ResolveOutcome::NotFound;
    }
    else if (candidates.size() == 1)
    {
        result.outcome = ResolveOutcome::Unique;
        result.candidates = std::move(candidates);
    }
    else
    {
        result.outcome = ResolveOutcome::Ambiguous;
        result.candidates = std::move(candidates);
    }
    return result;
}

}  // namespace

ResolveResult resolveSymbol(symhow::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter, const std::string* signatureFilter)
{
    if (!spec.empty() && spec.front() == '#')
    {
        const std::string idText = spec.substr(1);
        int64_t id = 0;
        const auto* begin = idText.data();
        const auto* end = idText.data() + idText.size();
        const auto [ptr, ec] = std::from_chars(begin, end, id);
        if (ec != std::errc{} || ptr != end)
        {
            return ResolveResult{ResolveOutcome::NotFound, {}};
        }

        std::vector<ResolvedSymbol> candidates;
        db.run(
            "SELECT id, type, serialized_name FROM node WHERE id = ?",
            [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, id); },
            [&](sqlite3_stmt* stmt) {
                candidates.push_back(ResolvedSymbol{sqlite3_column_int64(stmt, 0), sqlite3_column_int(stmt, 1), columnText(stmt, 2)});
            });
        return finalize(std::move(candidates));
    }

    const std::string prefilterKey = likePrefilterKey(spec);
    std::vector<ResolvedSymbol> candidates;
    db.run(
        "SELECT id, type, serialized_name FROM node WHERE serialized_name LIKE '%' || ? || '%'",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, prefilterKey.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            const int kind = sqlite3_column_int(stmt, 1);
            const std::string sname = columnText(stmt, 2);

            if (kindFilter != nullptr && !kindFilter->empty())
            {
                if (std::find(kindFilter->begin(), kindFilter->end(), kind) == kindFilter->end())
                {
                    return;
                }
            }

            const symhow::db::DecodedName decoded = symhow::db::decodeSerializedName(sname);
            const std::string qualified = decoded.qualifiedName();
            const std::string display = decoded.displayName();
            const std::string path = decoded.namePath();
            const std::string leaf = decoded.elements.empty() ? std::string() : decoded.elements.back().name;

            const bool nameMatches = spec == qualified || spec == display || spec == path || spec == leaf;
            if (!nameMatches)
            {
                return;
            }

            if (signatureFilter != nullptr && decoded.signature() != *signatureFilter)
            {
                return;
            }

            candidates.push_back(ResolvedSymbol{id, kind, sname});
        });

    return finalize(std::move(candidates));
}

ResolveResult resolveFilePath(symhow::db::Database& db, const std::string& spec)
{
    std::vector<ResolvedSymbol> exact;
    db.run(
        "SELECT id, path FROM file WHERE path = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, spec.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) {
            exact.push_back(ResolvedSymbol{sqlite3_column_int64(stmt, 0), symhow::db::NodeKind::FILE, columnText(stmt, 1)});
        });
    if (!exact.empty())
    {
        return finalize(std::move(exact));
    }

    std::vector<ResolvedSymbol> suffix;
    db.run(
        "SELECT id, path FROM file WHERE path LIKE '%' || ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, spec.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) {
            suffix.push_back(ResolvedSymbol{sqlite3_column_int64(stmt, 0), symhow::db::NodeKind::FILE, columnText(stmt, 1)});
        });
    return finalize(std::move(suffix));
}

}  // namespace symhow::query
