#pragma once

#include <cstdint>
#include <functional>
#include <string>
#include <unordered_map>
#include <vector>

struct sqlite3;
struct sqlite3_stmt;

// Read-only SQLite access to a Sourcetrail-schema (storage_version 25)
// database. Never opens for writing (SQLITE_OPEN_READONLY) -- the one
// exception, ensureIndices() below, deliberately lives OUTSIDE the Database
// class and opens its own short-lived read-write connection, so it's obvious
// at every call site which operations can possibly touch the file on disk.
namespace symhow::db
{

// Pins the schema version this whole query engine was written against; see
// Database's constructor. No migration logic -- hard-fail on mismatch.
constexpr int kExpectedStorageVersion = 25;

class Database
{
public:
    // Opens `path` read-only. Throws std::runtime_error if the file can't be
    // opened, doesn't have a `meta` table, has no `storage_version` row, or
    // that row's value isn't exactly "25" (kExpectedStorageVersion).
    //
    // If `verbosity` > 0, also checks (read-only, via sqlite_master) for the
    // 9 reader-performance indices `symhow index` creates at write time and logs
    // one warning per missing one -- this is diagnostic-only, never fatal,
    // and never auto-creates anything (see ensureIndices() for that).
    explicit Database(std::string path, int verbosity = 0);
    ~Database();

    Database(const Database&) = delete;
    Database& operator=(const Database&) = delete;
    Database(Database&&) = delete;
    Database& operator=(Database&&) = delete;

    const std::string& path() const
    {
        return m_path;
    }

    int storageVersion() const
    {
        return m_storageVersion;
    }

    sqlite3* handle() const
    {
        return m_handle;
    }

    using Binder = std::function<void(sqlite3_stmt*)>;
    using RowFn = std::function<void(sqlite3_stmt*)>;

    // Runs `sql` (preparing it once and caching the prepared statement for
    // reuse across calls with the same SQL text -- see the class comment on
    // m_stmtCache), invoking `bind` once to bind parameters (if any) and then
    // `onRow` once per result row. Throws std::runtime_error on any sqlite
    // error (including a non-SQLITE_DONE/SQLITE_ROW step result).
    void run(const std::string& sql, const Binder& bind, const RowFn& onRow) const;

    void run(const std::string& sql, const RowFn& onRow) const
    {
        run(sql, Binder{}, onRow);
    }

    // Chunked `WHERE col IN (?,?,...)` helper: `sqlTemplate` must contain the
    // literal marker "{IN}" exactly once, which is replaced with a
    // comma-joined "?,?,..." placeholder list sized to each chunk of `ids`
    // (`chunkSize` ids per chunk; SQLite's default SQLITE_LIMIT_VARIABLE_NUMBER
    // is comfortably above that). `extraBind`, if set, is invoked once per
    // chunk's prepared statement AFTER all the "{IN}" placeholders are bound,
    // receiving the next free 1-based parameter index -- e.g. for
    // "... IN ({IN}) AND (type & ?) != 0" with a 500-id chunk, extraBind is
    // called with index 501. Results from every chunk are delivered to the
    // same `onRow` callback (chunk boundaries are invisible to the caller;
    // "union the results in C++" per the chunked-IN design). No-op if `ids`
    // is empty.
    template <typename T>
    void runChunkedIn(
        const std::string& sqlTemplate,
        const std::vector<T>& ids,
        const std::function<void(sqlite3_stmt*, int)>& extraBind,
        const RowFn& onRow,
        std::size_t chunkSize = 500) const;

    template <typename T>
    void runChunkedIn(const std::string& sqlTemplate, const std::vector<T>& ids, const RowFn& onRow, std::size_t chunkSize = 500) const
    {
        runChunkedIn(sqlTemplate, ids, std::function<void(sqlite3_stmt*, int)>{}, onRow, chunkSize);
    }

private:
    sqlite3_stmt* prepareCached(const std::string& sql) const;

    std::string m_path;
    sqlite3* m_handle = nullptr;
    int m_storageVersion = 0;

    // Keyed by exact SQL text. runChunkedIn() generates a handful of distinct
    // SQL strings per logical query (one per distinct chunk size actually
    // used -- typically just `chunkSize` and one smaller remainder), so this
    // stays small and bounded for the lifetime of a single batch `symhow query`
    // invocation; there's no eviction because this process never runs long
    // enough for that to matter.
    mutable std::unordered_map<std::string, sqlite3_stmt*> m_stmtCache;
};

// Opens `dbPath` READ-WRITE, runs the same 9 `CREATE INDEX IF NOT EXISTS`
// statements `symhow index` already runs at write time (src/index/
// SrctrldbBackend.cpp's finish()), and closes it again. For a database
// written by `symhow index` this is a no-op (the indices already exist); it
// exists so a `.srctrldb` written by the real Sourcetrail GUI/indexer (which
// doesn't create these) can also be queried efficiently. Throws
// std::runtime_error on failure. Must be called BEFORE constructing the
// (always read-only) Database for the same path.
void ensureIndices(const std::string& dbPath);

// Names of the 9 indices ensureIndices() creates, for the missing-index
// warning below.
const std::vector<std::string>& expectedIndexNames();

// Which of expectedIndexNames() are NOT present in `db`'s sqlite_master.
// Empty if all are present (as they always are for a `symhow index`-written db).
std::vector<std::string> missingIndexNames(const Database& db);

}  // namespace symhow::db

#include "symhow/db/Database.impl.h"
