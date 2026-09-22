#include "kg/db/Database.h"

#include <sqlite3.h>

#include <algorithm>
#include <stdexcept>
#include <utility>

#include "kg/common/Log.h"
#include "kg/db/WritableDb.h"

namespace kg::db
{

namespace
{

// Exactly the 9 statements src/index/SrctrldbBackend.cpp's finish() runs
// post-write (kIndexStatements there) -- duplicated here rather than shared
// via a common header, per the Phase 4 instructions (kg_index is off-limits
// to this phase, and this is a small, stable, copy-once list).
const std::vector<std::pair<std::string, std::string>>& indexDefinitions()
{
    static const std::vector<std::pair<std::string, std::string>> kDefs = {
        {"kg_edge_source_idx", "CREATE INDEX IF NOT EXISTS kg_edge_source_idx ON edge(source_node_id);"},
        {"kg_edge_target_idx", "CREATE INDEX IF NOT EXISTS kg_edge_target_idx ON edge(target_node_id);"},
        {"kg_edge_type_idx", "CREATE INDEX IF NOT EXISTS kg_edge_type_idx ON edge(type);"},
        {"kg_node_type_idx", "CREATE INDEX IF NOT EXISTS kg_node_type_idx ON node(type);"},
        {"kg_srcloc_file_idx", "CREATE INDEX IF NOT EXISTS kg_srcloc_file_idx ON source_location(file_node_id);"},
        {"kg_occurrence_elem_idx", "CREATE INDEX IF NOT EXISTS kg_occurrence_elem_idx ON occurrence(element_id);"},
        {"kg_occurrence_loc_idx", "CREATE INDEX IF NOT EXISTS kg_occurrence_loc_idx ON occurrence(source_location_id);"},
        {"kg_file_path_idx", "CREATE INDEX IF NOT EXISTS kg_file_path_idx ON file(path);"},
        {"kg_symbol_defkind_idx", "CREATE INDEX IF NOT EXISTS kg_symbol_defkind_idx ON symbol(definition_kind);"},
    };
    return kDefs;
}

}  // namespace

const std::vector<std::string>& expectedIndexNames()
{
    static const std::vector<std::string> kNames = [] {
        std::vector<std::string> names;
        for (const auto& [name, stmt] : indexDefinitions())
        {
            names.push_back(name);
        }
        return names;
    }();
    return kNames;
}

void ensureIndices(const std::string& dbPath)
{
    try
    {
        WritableDb db(dbPath, /*foreignKeys=*/false);
        db.begin();
        for (const auto& [name, stmt] : indexDefinitions())
        {
            db.exec(stmt);
        }
        db.commit();
    }
    catch (const std::exception& e)
    {
        throw std::runtime_error("kg query --ensure-indices: " + std::string(e.what()));
    }
}

std::vector<std::string> missingIndexNames(const Database& db)
{
    std::vector<std::string> present;
    db.run("SELECT name FROM sqlite_master WHERE type = 'index'", [&](sqlite3_stmt* stmt) {
        const unsigned char* text = sqlite3_column_text(stmt, 0);
        present.emplace_back(text != nullptr ? reinterpret_cast<const char*>(text) : "");
    });

    std::vector<std::string> missing;
    for (const std::string& expected : expectedIndexNames())
    {
        if (std::find(present.begin(), present.end(), expected) == present.end())
        {
            missing.push_back(expected);
        }
    }
    return missing;
}

Database::Database(std::string path, int verbosity) : m_path(std::move(path))
{
    const int rc = sqlite3_open_v2(m_path.c_str(), &m_handle, SQLITE_OPEN_READONLY, nullptr);
    if (rc != SQLITE_OK)
    {
        const std::string msg =
            "failed to open '" + m_path + "' (read-only): " + (m_handle != nullptr ? sqlite3_errmsg(m_handle) : sqlite3_errstr(rc));
        if (m_handle != nullptr)
        {
            sqlite3_close(m_handle);
            m_handle = nullptr;
        }
        throw std::runtime_error(msg);
    }

    bool foundVersionRow = false;
    std::string versionText;
    try
    {
        run("SELECT value FROM meta WHERE key = 'storage_version'", [&](sqlite3_stmt* stmt) {
            foundVersionRow = true;
            const unsigned char* text = sqlite3_column_text(stmt, 0);
            versionText = text != nullptr ? reinterpret_cast<const char*>(text) : "";
        });
    }
    catch (const std::exception& e)
    {
        sqlite3_close(m_handle);
        m_handle = nullptr;
        throw std::runtime_error("'" + m_path + "' does not look like a Sourcetrail-schema database (no readable `meta` table): " + e.what());
    }

    if (!foundVersionRow)
    {
        sqlite3_close(m_handle);
        m_handle = nullptr;
        throw std::runtime_error("'" + m_path + "' has no `meta.storage_version` row -- not a valid Sourcetrail-schema database");
    }

    try
    {
        m_storageVersion = std::stoi(versionText);
    }
    catch (const std::exception&)
    {
        m_storageVersion = -1;
    }

    if (m_storageVersion != kExpectedStorageVersion)
    {
        const std::string msg = "'" + m_path + "' has storage_version '" + versionText + "', but kg query only supports " +
                                 std::to_string(kExpectedStorageVersion) + " (no migration support -- re-index with this build's `kg index`)";
        sqlite3_close(m_handle);
        m_handle = nullptr;
        throw std::runtime_error(msg);
    }

    if (verbosity > 0)
    {
        for (const std::string& name : missingIndexNames(*this))
        {
            kg::common::logInfo("kg query: warning: expected index '" + name + "' is missing (run with --ensure-indices to create it)");
        }
    }
}

Database::~Database()
{
    for (auto& [sql, stmt] : m_stmtCache)
    {
        sqlite3_finalize(stmt);
    }
    if (m_handle != nullptr)
    {
        sqlite3_close(m_handle);
    }
}

sqlite3_stmt* Database::prepareCached(const std::string& sql) const
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

void Database::run(const std::string& sql, const Binder& bind, const RowFn& onRow) const
{
    sqlite3_stmt* stmt = prepareCached(sql);
    sqlite3_reset(stmt);
    sqlite3_clear_bindings(stmt);
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
        throw std::runtime_error("sqlite error running '" + sql + "': " + sqlite3_errmsg(m_handle));
    }
}

}  // namespace kg::db
