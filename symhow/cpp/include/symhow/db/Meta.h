#pragma once

#include <optional>
#include <string>

struct sqlite3;

namespace symhow::db
{

class WritableDb;

// Reads `meta`'s value for `key` (via a raw read-only-capable handle -- works
// against both symhow::db::Database::handle() and symhow::db::WritableDb::handle()).
// Returns std::nullopt if there's no such row (or `meta` doesn't exist).
std::optional<std::string> getMeta(sqlite3* handle, const std::string& key);

// Idempotently sets `meta`'s value for `key` (DELETE then INSERT, since
// `meta.key` has no UNIQUE constraint in the Sourcetrail schema).
void setMeta(WritableDb& db, const std::string& key, const std::string& value);

// Deletes all `meta` rows for `key`, if any.
void deleteMeta(WritableDb& db, const std::string& key);

}  // namespace symhow::db
