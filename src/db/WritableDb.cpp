#include "kg/db/WritableDb.h"

#include <sqlite3.h>

#include <stdexcept>

namespace kg::db
{

WritableDb::WritableDb(std::string path, bool foreignKeys) : m_path(std::move(path))
{
    const int rc = sqlite3_open_v2(m_path.c_str(), &m_handle, SQLITE_OPEN_READWRITE, nullptr);
    if (rc != SQLITE_OK)
    {
        const std::string msg =
            "failed to open '" + m_path + "' (read-write): " + (m_handle != nullptr ? sqlite3_errmsg(m_handle) : sqlite3_errstr(rc));
        if (m_handle != nullptr)
        {
            sqlite3_close(m_handle);
            m_handle = nullptr;
        }
        throw std::runtime_error(msg);
    }

    if (foreignKeys)
    {
        exec("PRAGMA foreign_keys=ON;");
    }
}

WritableDb::~WritableDb()
{
    if (m_inTransaction)
    {
        char* errMsg = nullptr;
        sqlite3_exec(m_handle, "ROLLBACK;", nullptr, nullptr, &errMsg);
        if (errMsg != nullptr)
        {
            sqlite3_free(errMsg);
        }
    }
    for (auto& [sql, stmt] : m_stmtCache)
    {
        sqlite3_finalize(stmt);
    }
    if (m_handle != nullptr)
    {
        sqlite3_close(m_handle);
    }
}

void WritableDb::exec(const std::string& sql)
{
    char* errMsg = nullptr;
    const int rc = sqlite3_exec(m_handle, sql.c_str(), nullptr, nullptr, &errMsg);
    if (rc != SQLITE_OK)
    {
        const std::string msg = "sqlite error running '" + sql + "': " + (errMsg != nullptr ? errMsg : "unknown error");
        if (errMsg != nullptr)
        {
            sqlite3_free(errMsg);
        }
        throw std::runtime_error(msg);
    }
}

sqlite3_stmt* WritableDb::prepareCached(const std::string& sql)
{
    auto it = m_stmtCache.find(sql);
    if (it != m_stmtCache.end())
    {
        return it->second;
    }
    sqlite3_stmt* stmt = nullptr;
    const int rc = sqlite3_prepare_v2(m_handle, sql.c_str(), -1, &stmt, nullptr);
    if (rc != SQLITE_OK)
    {
        throw std::runtime_error("failed to prepare statement '" + sql + "': " + sqlite3_errmsg(m_handle));
    }
    m_stmtCache.emplace(sql, stmt);
    return stmt;
}

namespace
{

// Resets `stmt` (dropping it out of "still stepping" state) no matter how
// runPrepared() below exits -- including via an exception thrown either by
// sqlite3_step() itself or by a `bind`/`onRow` callback. Without this, an
// exception mid-step leaves the CACHED statement (it lives in m_stmtCache,
// reused across calls, not finalized here) holding an implicit read/write
// lock; the destructor's unconditional ROLLBACK can then fail with
// SQLITE_BUSY instead of actually rolling back.
struct StmtResetGuard
{
    sqlite3_stmt* stmt;
    ~StmtResetGuard()
    {
        sqlite3_reset(stmt);
        sqlite3_clear_bindings(stmt);
    }
};

void runPrepared(sqlite3* handle, sqlite3_stmt* stmt, const std::string& sql, const WritableDb::Binder& bind, const WritableDb::RowFn& onRow)
{
    sqlite3_reset(stmt);
    sqlite3_clear_bindings(stmt);
    StmtResetGuard guard{stmt};
    if (bind)
    {
        bind(stmt);
    }

    int rc = 0;
    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW)
    {
        if (onRow)
        {
            onRow(stmt);
        }
    }
    if (rc != SQLITE_DONE)
    {
        throw std::runtime_error("sqlite error running '" + sql + "': " + sqlite3_errmsg(handle));
    }
}
}  // namespace

void WritableDb::execBound(const std::string& sql, const Binder& bind)
{
    runPrepared(m_handle, prepareCached(sql), sql, bind, RowFn{});
}

void WritableDb::query(const std::string& sql, const RowFn& onRow)
{
    runPrepared(m_handle, prepareCached(sql), sql, Binder{}, onRow);
}

void WritableDb::query(const std::string& sql, const Binder& bind, const RowFn& onRow)
{
    runPrepared(m_handle, prepareCached(sql), sql, bind, onRow);
}

int WritableDb::changes() const
{
    return sqlite3_changes(m_handle);
}

void WritableDb::begin()
{
    exec("BEGIN;");
    m_inTransaction = true;
}

void WritableDb::commit()
{
    exec("COMMIT;");
    m_inTransaction = false;
}

void WritableDb::rollback()
{
    exec("ROLLBACK;");
    m_inTransaction = false;
}

bool WritableDb::tableExists(const std::string& name)
{
    bool found = false;
    query(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
        [&](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, name.c_str(), -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt*) { found = true; });
    return found;
}

}  // namespace kg::db
