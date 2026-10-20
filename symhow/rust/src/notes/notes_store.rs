//! Phase 2 of docs/NOTES_DESIGN.md: the `symhow_note*` cache schema and the
//! read-path overlay (`open_notes()`) that `symhow notes show`/`status`/
//! `symbols` use. `symhow notes sync` (`sync.rs`) is the only thing that
//! WRITES this schema; everything here is read-only against an already-open
//! `Database`. Mirrors `symhow::notes::NotesStore.{h,cpp}`.

use std::collections::HashMap;
use std::path::Path;

use crate::common::Result;
use crate::db::meta::get_meta;
use crate::db::Database;
use crate::index::clangutil::{fnv1a64, hex16};

use super::body_hash;
use super::markdown::parse_notes_file;
use super::sync::{collect_md_files, match_block, relative_generic, MatchKind, MatchOutcome};

/// DDL, `CREATE TABLE IF NOT EXISTS` -- run by `sync_notes()` (the only
/// writer) via a `WritableDb` before its rebuild transaction. Kept here (not
/// in sync.rs) since `open_notes()` below needs the exact same column
/// set/names to read the tables back.
pub const K_CREATE_NOTE_TABLE_SQL: &str = "
CREATE TABLE IF NOT EXISTS symhow_note(
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
";

pub const K_CREATE_NOTE_FILE_TABLE_SQL: &str = "
CREATE TABLE IF NOT EXISTS symhow_note_file(
  notes_path TEXT PRIMARY KEY,
  mtime_ns   INTEGER NOT NULL,
  content_hash TEXT NOT NULL);
";

pub const K_CREATE_NOTE_PROBLEM_TABLE_SQL: &str = "
CREATE TABLE IF NOT EXISTS symhow_note_problem(
  notes_path TEXT, line INTEGER, heading TEXT, kind TEXT, message TEXT);
";

/// One `symhow_note` row, decoded into Rust types (pre/post/bugs/suggestions
/// JSON arrays parsed back into vectors).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredNote {
    pub serialized_name: String,
    pub qualified_name: String,
    pub notes_path: String,
    pub heading_line: i32,

    pub brief: String,
    pub what: String,
    pub how: String,
    pub signature_text: String,
    pub pre: Vec<String>,
    pub post: Vec<String>,
    pub bugs: Vec<String>,
    pub suggestions: Vec<String>,

    pub filled: bool,
    pub note_hash: String,
    pub note_commit: String,
    pub current_body_hash: String,
    pub stale: bool,
    pub synced_at: String,
}

/// Read-path view over the notes cache: `symhow_note` loaded into memory,
/// overlaid (in memory only -- never written back to the DB) with any
/// `*.md` file under the notes dir whose on-disk content-hash no longer
/// matches its `symhow_note_file` row (or that's new since the last sync),
/// so `symhow notes show`/`status` never return data staler than what's on
/// disk. See `open_notes()` below for exactly how overlaid blocks are
/// matched (the same `match_block()` sync uses).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteLookup {
    /// Keyed by `serialized_name` (the same string
    /// `symhow::query::ResolvedSymbol::serialized_name` carries), so callers
    /// that already resolved a symbol via `resolve_symbol()` can look its
    /// note up directly.
    pub by_serialized_name: HashMap<String, StoredNote>,

    /// Relative (to the notes dir) paths of every file the overlay pass
    /// found changed-on-disk-since-last-sync or brand new -- callers print
    /// "notes cache out of date (N files changed) -- run symhow notes sync"
    /// on stderr when non-empty.
    pub overlay_files: Vec<String>,
}

fn json_array_to_vec(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<String>>(s) {
        Ok(v) => v,
        Err(_) => Vec::new(),
    }
}

/// Same content-hash formula `sync_notes()` writes into
/// symhow_note_file.content_hash -- 16 lowercase hex digits of FNV-1a-64 over
/// the raw file bytes.
pub(super) fn content_hash_of(bytes: &str) -> String {
    hex16(fnv1a64(bytes))
}

fn table_exists(db: &Database, name: &str) -> Result<bool> {
    let mut found = false;
    db.run(
        "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
        [name],
        |_row| {
            found = true;
            Ok(())
        },
    )?;
    Ok(found)
}

fn note_from_block(
    db: &Database,
    notes_path: &str,
    b: &super::note_model::NoteBlock,
    mo: &MatchOutcome,
) -> Result<StoredNote> {
    Ok(StoredNote {
        serialized_name: mo.serialized_name.clone(),
        qualified_name: mo.qualified_name.clone(),
        notes_path: notes_path.to_string(),
        heading_line: b.heading_line,
        brief: b.brief.clone(),
        what: b.what.text.clone(),
        how: b.how.text.clone(),
        signature_text: mo.signature.clone(),
        pre: b.pre.items.clone(),
        post: b.post.items.clone(),
        bugs: b.bugs.items.clone(),
        suggestions: b.suggestions.items.clone(),
        filled: b.filled(),
        note_hash: b.body_hash.clone(),
        note_commit: b.commit.clone(),
        current_body_hash: body_hash::body_hash_for_node(db, mo.node_id)?,
        stale: false,
        synced_at: String::new(), // overlay entry, not persisted -- never synced
    })
}

impl StoredNote {
    /// `stale` is the only field `note_from_block()` doesn't set directly
    /// (it needs `current_body_hash`, computed from the DB) -- mirror
    /// NotesStore.cpp's `!noteHash.empty() && !currentBodyHash.empty()
    /// && noteHash != currentBodyHash` here so callers don't repeat it.
    fn with_stale(mut self) -> Self {
        self.stale = !self.note_hash.is_empty() && !self.current_body_hash.is_empty() && self.note_hash != self.current_body_hash;
        self
    }
}

/// Loads `symhow_note` (empty NoteLookup if the table doesn't exist yet -- i.e.
/// `symhow notes sync` has never run), then compares every
/// `symhow_note_file` row's content_hash against disk (notes dir read from
/// `meta.symhow_notes_dir`) and also walks the notes dir for `*.md` files with
/// NO `symhow_note_file` row at all (new since last sync). For every such
/// file, re-parses it and re-runs the same matching logic `sync_notes()`
/// uses (`match_block()`) to overlay its blocks into `by_serialized_name`,
/// replacing any stale entries and adding new ones -- entirely in memory,
/// this never writes to the DB. If `meta.symhow_notes_dir` is absent (sync
/// never ran), returns an empty NoteLookup with no overlay attempted
/// (nothing to compare disk against).
pub fn open_notes(db: &Database) -> Result<NoteLookup> {
    let mut lookup = NoteLookup::default();

    if !table_exists(db, "symhow_note")? {
        return Ok(lookup);
    }

    db.run(
        "SELECT serialized_name, qualified_name, notes_path, heading_line, brief, what, how, signature_text, \
         pre_json, post_json, bugs_json, suggestions_json, filled, note_hash, note_commit, current_body_hash, \
         stale, synced_at FROM symhow_note",
        [],
        |row| {
            let n = StoredNote {
                serialized_name: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                qualified_name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                notes_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                heading_line: row.get::<_, i32>(3)?,
                brief: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                what: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                how: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                signature_text: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                pre: json_array_to_vec(&row.get::<_, Option<String>>(8)?.unwrap_or_default()),
                post: json_array_to_vec(&row.get::<_, Option<String>>(9)?.unwrap_or_default()),
                bugs: json_array_to_vec(&row.get::<_, Option<String>>(10)?.unwrap_or_default()),
                suggestions: json_array_to_vec(&row.get::<_, Option<String>>(11)?.unwrap_or_default()),
                filled: row.get::<_, Option<i32>>(12)?.unwrap_or(0) != 0,
                note_hash: row.get::<_, Option<String>>(13)?.unwrap_or_default(),
                note_commit: row.get::<_, Option<String>>(14)?.unwrap_or_default(),
                current_body_hash: row.get::<_, Option<String>>(15)?.unwrap_or_default(),
                stale: row.get::<_, Option<i32>>(16)?.unwrap_or(0) != 0,
                synced_at: row.get::<_, Option<String>>(17)?.unwrap_or_default(),
            };
            lookup.by_serialized_name.insert(n.serialized_name.clone(), n);
            Ok(())
        },
    )?;

    let Some(notes_root) = get_meta(db.conn(), "symhow_notes_dir").filter(|d| !d.is_empty()) else {
        return Ok(lookup);
    };
    let notes_root = Path::new(&notes_root);
    if !notes_root.is_dir() {
        return Ok(lookup);
    }

    // notes_path -> content_hash as of the last sync.
    let mut last_synced_hash: HashMap<String, String> = HashMap::new();
    if table_exists(db, "symhow_note_file")? {
        db.run("SELECT notes_path, content_hash FROM symhow_note_file", [], |row| {
            last_synced_hash.insert(
                row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            );
            Ok(())
        })?;
    }

    let mut md_files: Vec<String> = Vec::new();
    collect_md_files(notes_root, &mut md_files)?;
    md_files.sort();

    for full in &md_files {
        let full_path = Path::new(full);
        let rel_path = relative_generic(full_path, notes_root);
        let content = super::sync::read_file_lossy(full_path);
        let hash = content_hash_of(&content);

        let changed = match last_synced_hash.get(&rel_path) {
            Some(last) => *last != hash,
            None => true,
        };
        if !changed {
            continue;
        }

        lookup.overlay_files.push(rel_path.clone());

        // Drop every cached entry that used to belong to this file (it may
        // have lost blocks entirely) before re-adding whatever the current
        // on-disk content matches to.
        lookup
            .by_serialized_name
            .retain(|_, entry| entry.notes_path != rel_path);

        let parsed_file = parse_notes_file(&content, &rel_path, false);
        for b in &parsed_file.blocks {
            let mo = match_block(db, b)?;
            if mo.kind != MatchKind::Matched {
                continue;
            }
            let n = note_from_block(db, &rel_path, b, &mo)?.with_stale();
            lookup.by_serialized_name.insert(mo.serialized_name.clone(), n);
        }
    }

    lookup.overlay_files.sort();
    Ok(lookup)
}
