#include <catch2/catch_test_macros.hpp>

#include <cstdio>
#include <filesystem>
#include <stdexcept>

#include <sqlite3.h>

#include "kg/db/Meta.h"
#include "kg/db/WritableDb.h"

using kg::db::WritableDb;

namespace
{

// Creates a fresh, empty sqlite file at `path` (WritableDb requires the
// file to already exist -- it never creates one via SQLITE_OPEN_CREATE).
void createEmptyDb(const std::string& path)
{
    sqlite3* db = nullptr;
    REQUIRE(sqlite3_open_v2(path.c_str(), &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nullptr) == SQLITE_OK);
    sqlite3_close(db);
}

std::string tempDbPath(const char* name)
{
    return (std::filesystem::temp_directory_path() / name).string();
}

}  // namespace

TEST_CASE("writable_db: exec runs DDL and run()/changes() work against bound INSERT/SELECT")
{
    const std::string path = tempDbPath("kg_test_writable_db_basic.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    WritableDb db(path);
    db.exec("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
    db.execBound(
        "INSERT INTO t(v) VALUES (?)",
        [](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, "hello", -1, SQLITE_TRANSIENT); });
    REQUIRE(db.changes() == 1);

    std::string seen;
    db.query("SELECT v FROM t", [&](sqlite3_stmt* stmt) {
        seen = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
    });
    REQUIRE(seen == "hello");

    REQUIRE(db.tableExists("t"));
    REQUIRE_FALSE(db.tableExists("nonexistent_table"));

    std::filesystem::remove(path);
}

TEST_CASE("writable_db: exec throws std::runtime_error with the sqlite message on failure")
{
    const std::string path = tempDbPath("kg_test_writable_db_error.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    WritableDb db(path);
    REQUIRE_THROWS_AS(db.exec("SELECT * FROM this_table_does_not_exist"), std::runtime_error);

    std::filesystem::remove(path);
}

TEST_CASE("writable_db: begin/commit persists, begin/rollback (via destructor) discards")
{
    const std::string path = tempDbPath("kg_test_writable_db_txn.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    {
        WritableDb db(path);
        db.exec("CREATE TABLE t(v INTEGER)");
        db.begin();
        db.exec("INSERT INTO t(v) VALUES (1)");
        db.commit();
    }

    {
        WritableDb db(path);
        db.begin();
        db.exec("INSERT INTO t(v) VALUES (2)");
        // No commit -- destructor should roll this back.
    }

    {
        WritableDb db(path);
        int count = 0;
        db.query("SELECT count(*) FROM t", [&](sqlite3_stmt* stmt) { count = sqlite3_column_int(stmt, 0); });
        REQUIRE(count == 1);
    }

    std::filesystem::remove(path);
}

TEST_CASE("writable_db: explicit rollback() discards")
{
    const std::string path = tempDbPath("kg_test_writable_db_rollback.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    WritableDb db(path);
    db.exec("CREATE TABLE t(v INTEGER)");
    db.begin();
    db.exec("INSERT INTO t(v) VALUES (1)");
    db.rollback();

    int count = -1;
    db.query("SELECT count(*) FROM t", [&](sqlite3_stmt* stmt) { count = sqlite3_column_int(stmt, 0); });
    REQUIRE(count == 0);

    std::filesystem::remove(path);
}

TEST_CASE("writable_db + meta: set/get/overwrite/delete round-trip")
{
    const std::string path = tempDbPath("kg_test_writable_db_meta.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    WritableDb db(path);
    db.exec("CREATE TABLE meta(id INTEGER PRIMARY KEY, key TEXT, value TEXT)");

    REQUIRE_FALSE(kg::db::getMeta(db.handle(), "foo").has_value());

    kg::db::setMeta(db, "foo", "bar");
    REQUIRE(kg::db::getMeta(db.handle(), "foo") == "bar");

    kg::db::setMeta(db, "foo", "baz");
    REQUIRE(kg::db::getMeta(db.handle(), "foo") == "baz");
    // setMeta is DELETE-then-INSERT (meta.key has no UNIQUE constraint), so
    // there must be exactly one row for the key after an overwrite.
    int rows = -1;
    db.query(
        "SELECT count(*) FROM meta WHERE key = ?",
        [](sqlite3_stmt* stmt) { sqlite3_bind_text(stmt, 1, "foo", -1, SQLITE_TRANSIENT); },
        [&](sqlite3_stmt* stmt) { rows = sqlite3_column_int(stmt, 0); });
    REQUIRE(rows == 1);

    kg::db::deleteMeta(db, "foo");
    REQUIRE_FALSE(kg::db::getMeta(db.handle(), "foo").has_value());

    std::filesystem::remove(path);
}

TEST_CASE("writable_db: foreignKeys=true actually enforces ON DELETE CASCADE")
{
    // Production WritableDb constructions all pass foreignKeys=false (SrctrldbBackend::finish()
    // never deletes rows, so it doesn't need the pragma) -- this proves the
    // `foreignKeys` constructor argument itself does what it claims, so a
    // future caller that DOES need cascading deletes (e.g. Phase 3's
    // `kg update` invalidation) can rely on it.
    const std::string path = tempDbPath("kg_test_writable_db_fk_cascade.sqlite");
    std::filesystem::remove(path);
    createEmptyDb(path);

    WritableDb db(path, /*foreignKeys=*/true);
    db.exec("CREATE TABLE parent(id INTEGER PRIMARY KEY)");
    db.exec(
        "CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER, "
        "FOREIGN KEY(parent_id) REFERENCES parent(id) ON DELETE CASCADE)");
    db.exec("INSERT INTO parent(id) VALUES (1)");
    db.exec("INSERT INTO child(id, parent_id) VALUES (10, 1)");

    int childCountBefore = -1;
    db.query("SELECT count(*) FROM child", [&](sqlite3_stmt* stmt) { childCountBefore = sqlite3_column_int(stmt, 0); });
    REQUIRE(childCountBefore == 1);

    db.exec("DELETE FROM parent WHERE id = 1");

    int childCountAfter = -1;
    db.query("SELECT count(*) FROM child", [&](sqlite3_stmt* stmt) { childCountAfter = sqlite3_column_int(stmt, 0); });
    REQUIRE(childCountAfter == 0);

    std::filesystem::remove(path);
}
