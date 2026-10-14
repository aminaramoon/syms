#include "symhow/db/Meta.h"

#include <sqlite3.h>

#include "symhow/db/WritableDb.h"

namespace symhow::db
{

std::optional<std::string> getMeta(sqlite3* handle, const std::string& key)
{
    if (handle == nullptr)
    {
        return std::nullopt;
    }
    sqlite3_stmt* stmt = nullptr;
    if (sqlite3_prepare_v2(handle, "SELECT value FROM meta WHERE key = ?", -1, &stmt, nullptr) != SQLITE_OK)
    {
        return std::nullopt;
    }
    sqlite3_bind_text(stmt, 1, key.c_str(), -1, SQLITE_TRANSIENT);
    std::optional<std::string> result;
    if (sqlite3_step(stmt) == SQLITE_ROW)
    {
        const unsigned char* text = sqlite3_column_text(stmt, 0);
        result = text != nullptr ? reinterpret_cast<const char*>(text) : "";
    }
    sqlite3_finalize(stmt);
    return result;
}

void setMeta(WritableDb& db, const std::string& key, const std::string& value)
{
    db.execBound(
        "DELETE FROM meta WHERE key = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, key.c_str(), -1, SQLITE_TRANSIENT); });
    db.execBound(
        "INSERT INTO meta(key, value) VALUES(?, ?)",
        [&](sqlite3_stmt* stmt) {
            sqlite3_bind_text(stmt, 1, key.c_str(), -1, SQLITE_TRANSIENT);
            sqlite3_bind_text(stmt, 2, value.c_str(), -1, SQLITE_TRANSIENT);
        });
}

void deleteMeta(WritableDb& db, const std::string& key)
{
    db.execBound(
        "DELETE FROM meta WHERE key = ?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, key.c_str(), -1, SQLITE_TRANSIENT); });
}

}  // namespace symhow::db
