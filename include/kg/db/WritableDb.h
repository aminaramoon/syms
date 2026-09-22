#pragma once

#include <functional>
#include <string>
#include <unordered_map>

struct sqlite3;
struct sqlite3_stmt;

// RAII wrapper around a READ-WRITE sqlite3 handle. Unlike kg::db::Database
// (always read-only), this is the one place kg is allowed to open a
// Sourcetrail-schema (or kg-private-table) database for writing outside the
// vendored SourcetrailDBWriter. Used by SrctrldbBackend::finish()'s
// post-write fixup section and by kg::db::ensureIndices().
namespace kg::db
{

class WritableDb
{
public:
    // Opens `path` SQLITE_OPEN_READWRITE (the file must already exist).
    // If `foreignKeys` is true (the default), immediately executes
    // `PRAGMA foreign_keys=ON;`. Throws std::runtime_error on failure.
    explicit WritableDb(std::string path, bool foreignKeys = true);
    ~WritableDb();

    WritableDb(const WritableDb&) = delete;
    WritableDb& operator=(const WritableDb&) = delete;
    WritableDb(WritableDb&&) = delete;
    WritableDb& operator=(WritableDb&&) = delete;

    // Executes `sql` directly (sqlite3_exec -- no bound parameters, may
    // contain multiple statements). Throws std::runtime_error with the
    // sqlite error message on failure.
    void exec(const std::string& sql);

    using Binder = std::function<void(sqlite3_stmt*)>;
    using RowFn = std::function<void(sqlite3_stmt*)>;

    // NOTE on this API's shape: `Binder` and `RowFn` are both
    // `std::function<void(sqlite3_stmt*)>` -- the *same* C++ type -- so an
    // overload set containing both a 2-argument "bind, no rows expected"
    // entry point and a 2-argument "no bind, read rows" entry point would be
    // silently ambiguous-but-compiling: a bind-only call would resolve to
    // the row-callback overload, never bind its parameter, and (for an
    // INSERT/UPDATE) either write NULLs or throw a confusing NOT NULL
    // failure instead of a compile error. See docs/KNOWN_LIMITATIONS.md for
    // the real bug this caused during Phase 1. To make that class of
    // mistake impossible, there is no single `run()` overload set here --
    // instead there are four differently-named entry points, each with
    // exactly one arity/shape:

    // Prepares (and caches) `sql`, binds parameters via `bind`, and steps it
    // to completion. Use for INSERT/UPDATE/DELETE with bound parameters
    // where no result rows are expected (a row, if any were returned, is
    // silently ignored).
    void execBound(const std::string& sql, const Binder& bind);

    // Prepares (and caches) `sql` (no bound parameters) and invokes `onRow`
    // once per result row.
    void query(const std::string& sql, const RowFn& onRow);

    // Prepares (and caches) `sql`, binds parameters via `bind`, and invokes
    // `onRow` once per result row -- mirrors kg::db::Database::run's
    // signature/behavior, but against the writable handle.
    void query(const std::string& sql, const Binder& bind, const RowFn& onRow);

    int changes() const;

    void begin();
    void commit();
    void rollback();

    sqlite3* handle() const { return m_handle; }

    bool tableExists(const std::string& name);

private:
    sqlite3_stmt* prepareCached(const std::string& sql);

    std::string m_path;
    sqlite3* m_handle = nullptr;
    bool m_inTransaction = false;
    std::unordered_map<std::string, sqlite3_stmt*> m_stmtCache;
};

}  // namespace kg::db
