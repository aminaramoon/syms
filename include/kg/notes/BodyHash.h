#pragma once

#include <cstdint>
#include <string>

#include "kg/db/Database.h"

// Phase 2 of docs/NOTES_DESIGN.md: computes the "body hash" a note is
// anchored to -- a hash of the whitespace-normalized text of a symbol's
// definition span, read straight out of `filecontent`. Used by both
// `kg notes sync` (writes it into `kg_note.current_body_hash`) and the
// read-path overlay (NotesStore.h) to detect staleness without touching the
// cache.
namespace kg::notes
{

// Picks the node's SCOPE occurrence with the largest line span (ties broken
// by lexicographically smallest file path, then smallest start_line); if no
// SCOPE occurrence exists, falls back to the TOKEN occurrence's full line.
// Slices that span's text out of `filecontent.content`, whitespace-
// normalizes it (runs of whitespace collapsed to one space, trimmed), and
// hashes it with FNV-1a-64 (kg::index::clangutil::fnv1a64), rendered as 16
// lowercase hex digits. Returns "" if the node has no indexed location, or
// the location's file has no `filecontent` row.
std::string bodyHashForNode(kg::db::Database& db, int64_t nodeId);

// Reads `meta.kg_repo_commit` (empty string if absent).
std::string currentCommit(const kg::db::Database& db);

}  // namespace kg::notes
