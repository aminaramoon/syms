#pragma once

#include <cstdint>
#include <string>
#include <unordered_map>
#include <vector>

#include "kg/db/Database.h"

// Phase 2 of docs/NOTES_DESIGN.md: the `kg_note*` cache schema and the
// read-path overlay (`openNotes()`) that `kg notes show`/`status`/`symbols`
// use. `kg notes sync` (Sync.h) is the only thing that WRITES this schema;
// everything here is read-only against an already-open kg::db::Database.
namespace kg::notes
{

// DDL, `CREATE TABLE IF NOT EXISTS` -- run by kg::notes::syncNotes() (the
// only writer) via a kg::db::WritableDb before its rebuild transaction.
// Kept here (not in Sync.cpp) since NotesStore.cpp's openNotes() needs the
// exact same column set/names to read the tables back.
inline constexpr const char* kCreateNoteTableSql = R"sql(
CREATE TABLE IF NOT EXISTS kg_note(
  serialized_name TEXT PRIMARY KEY,
  qualified_name  TEXT NOT NULL,
  notes_path      TEXT NOT NULL,
  heading_line    INTEGER NOT NULL,
  brief TEXT, what TEXT, how TEXT, signature_text TEXT,
  pre_json TEXT, post_json TEXT, bugs_json TEXT, suggestions_json TEXT,
  filled          INTEGER NOT NULL,
  note_hash       TEXT,
  note_commit     TEXT,
  current_body_hash TEXT,
  stale           INTEGER NOT NULL DEFAULT 0,
  synced_at       TEXT NOT NULL);
)sql";

inline constexpr const char* kCreateNoteFileTableSql = R"sql(
CREATE TABLE IF NOT EXISTS kg_note_file(
  notes_path TEXT PRIMARY KEY,
  mtime_ns   INTEGER NOT NULL,
  content_hash TEXT NOT NULL);
)sql";

inline constexpr const char* kCreateNoteProblemTableSql = R"sql(
CREATE TABLE IF NOT EXISTS kg_note_problem(
  notes_path TEXT, line INTEGER, heading TEXT, kind TEXT, message TEXT);
)sql";

// One `kg_note` row, decoded into C++ types (pre/post/bugs/suggestions JSON
// arrays parsed back into vectors).
struct StoredNote
{
    std::string serializedName;
    std::string qualifiedName;
    std::string notesPath;
    int headingLine = 0;

    std::string brief;
    std::string what;
    std::string how;
    std::string signatureText;
    std::vector<std::string> pre;
    std::vector<std::string> post;
    std::vector<std::string> bugs;
    std::vector<std::string> suggestions;

    bool filled = false;
    std::string noteHash;
    std::string noteCommit;
    std::string currentBodyHash;
    bool stale = false;
    std::string syncedAt;
};

// Read-path view over the notes cache: `kg_note` loaded into memory,
// overlaid (in memory only -- never written back to the DB) with any
// `*.md` file under the notes dir whose on-disk mtime/content-hash no
// longer matches its `kg_note_file` row (or that's new since the last
// sync), so `kg notes show`/`status` never return data staler than what's
// on disk. See NotesStore.cpp's openNotes() for exactly how overlaid
// blocks are matched (same matchBlock() Sync.h uses).
struct NoteLookup
{
    // Keyed by `serialized_name` (the same string kg::query::ResolvedSymbol
    // ::serializedName carries), so callers that already resolved a symbol
    // via kg::query::resolveSymbol() can look its note up directly.
    std::unordered_map<std::string, StoredNote> bySerializedName;

    // Relative (to the notes dir) paths of every file the overlay pass
    // found changed-on-disk-since-last-sync or brand new -- callers print
    // "notes cache out of date (N files changed) -- run kg notes sync" on
    // stderr when non-empty.
    std::vector<std::string> overlayFiles;
};

// Loads `kg_note` (empty NoteLookup if the table doesn't exist yet -- i.e.
// `kg notes sync` has never run), then compares every `kg_note_file` row's
// mtime_ns/content_hash against disk (notes dir read from
// `meta.kg_notes_dir`) and also walks the notes dir for `*.md` files with NO
// `kg_note_file` row at all (new since last sync). For every such file,
// re-parses it and re-runs the same matching logic `syncNotes()` uses
// (Sync.h's matchBlock()) to overlay its blocks into `bySerializedName`,
// replacing any stale entries and adding new ones -- entirely in memory,
// this never writes to `db`. If `meta.kg_notes_dir` is absent (sync never
// ran), returns an empty NoteLookup with no overlay attempted (nothing to
// compare disk against).
NoteLookup openNotes(kg::db::Database& db);

}  // namespace kg::notes
