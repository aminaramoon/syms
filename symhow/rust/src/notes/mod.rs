//! Notes layer: markdown symbol-notes files anchored to indexed symbols.
//! Mirrors `symhow::notes` (see docs/NOTES_DESIGN.md):
//!
//! - [`markdown`]/[`note_model`] -- fence-aware markdown parse/render, no DB
//!   dependency (Phase 1);
//! - [`body_hash`] -- the FNV-1a-64 body hash a note's staleness anchor
//!   carries, computed from `filecontent` (Phase 2);
//! - [`notes_store`] -- the `symhow_note*` cache schema + read-path overlay
//!   (Phase 2);
//! - [`sync`] -- block matching + the `symhow notes sync` rebuild engine,
//!   with the CLI verb's `--json` envelope and exit-code contract (Phase 2);
//! - [`scaffold`] -- idempotent placeholder-block generation (Phase 3).

pub mod body_hash;
pub mod markdown;
pub mod note_model;
pub mod notes_store;
pub mod scaffold;
pub mod sync;

pub use body_hash::{body_hash_for_node, current_commit};
pub use markdown::{normalize_signature, parse_notes_file, placeholder_block_markdown, render_block, render_file};
pub use note_model::{ListSection, NoteBlock, NotesFile, Problem, TextSection, PLACEHOLDER};
pub use notes_store::{
    open_notes, NoteLookup, StoredNote, K_CREATE_NOTE_FILE_TABLE_SQL, K_CREATE_NOTE_PROBLEM_TABLE_SQL,
    K_CREATE_NOTE_TABLE_SQL,
};
pub use scaffold::{
    list_symbols, row_for_node, scaffold_notes, ScaffoldFileResult, ScaffoldOptions, ScaffoldResult, SymbolRow,
};
pub use sync::{
    match_block, sync_exit_code, sync_json, sync_notes, MatchKind, MatchOutcome, SyncCounts, SyncProblem,
    SyncResult,
};
