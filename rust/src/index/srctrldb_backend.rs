//! The SourcetrailDB persistence backend: consumes backend-neutral
//! `TuFactBatch`es and writes them through the vendored C++ writer bridge,
//! plus the symhow-private side tables (fingerprints, touched set,
//! symbol defs, symhow_tu) via its own rusqlite connection. Mirrors
//! `symhow::index::SrctrldbBackend` (src/index/SrctrldbBackend.cpp).
//!
//! This is the ONLY module in the Rust crate allowed to touch both
//! `symhow::model` types and `crate::srctrldb` (vendored-writer FFI) types --
//! it's the translation boundary between the backend-neutral fact model and
//! the vendored SourcetrailDB writer.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;

use rusqlite::params;

use crate::common::paths;
use crate::common::{Error, Result};
use crate::db::enums::{definition_kind, node_kind};
use crate::db::meta::delete_meta;
use crate::db::WritableDb;
use crate::index::provenance::{K_SYMHOW_FILE_FINGERPRINT_DDL, K_SYMHOW_TU_DDL};
use crate::model::{
    kind_rank, DefinitionKind, ElementClass, FileFact, IndexBackend, IndexSummary, LocationKind,
    LocalId, Name, ReferenceKind, SymbolFact, SymbolKind, TuFactBatch, K_NO_LOCAL,
};
use crate::srctrldb::{
    serialize_name_hierarchy, ReferenceKindInt, SymhowNameElement, SymhowSourceRange, Writer,
};

// --- translation of model enums to the vendored writer's on-disk ints ------

/// model::SymbolKind -> the sourcetrail *node-kind int* (NodeKind.h).
/// Unknown/Type (and defensively File, which should never reach here --
/// see ensure_symbol_node()) fall back to TYPE: the closest match in
/// sourcetrail::SymbolKind, and -- crucially -- still a *real* kind, so the
/// underlying node never stays NodeKind::UNKNOWN for a symbol we chose to
/// record.
fn translate_symbol_kind(kind: SymbolKind) -> i32 {
    match kind {
        SymbolKind::Namespace => node_kind::NAMESPACE,
        SymbolKind::Struct => node_kind::STRUCT,
        SymbolKind::Class => node_kind::CLASS,
        SymbolKind::Union => node_kind::UNION,
        SymbolKind::Enum => node_kind::ENUM,
        SymbolKind::EnumConstant => node_kind::ENUM_CONSTANT,
        SymbolKind::Field => node_kind::FIELD,
        SymbolKind::GlobalVariable => node_kind::GLOBAL_VARIABLE,
        SymbolKind::Function => node_kind::FUNCTION,
        SymbolKind::Method => node_kind::METHOD,
        SymbolKind::Typedef => node_kind::TYPEDEF,
        SymbolKind::Macro => node_kind::MACRO,
        // Type | Unknown | File (and defensively everything else)
        _ => node_kind::TYPE,
    }
}

/// model::ReferenceKind -> the vendored writer's ReferenceKind ordinal.
/// Unknown (and defensively anything unrecognized) falls back to USAGE.
fn translate_reference_kind(kind: ReferenceKind) -> ReferenceKindInt {
    match kind {
        ReferenceKind::TypeUsage => ReferenceKindInt::TypeUsage,
        ReferenceKind::Usage => ReferenceKindInt::Usage,
        ReferenceKind::Call => ReferenceKindInt::Call,
        ReferenceKind::Inheritance => ReferenceKindInt::Inheritance,
        ReferenceKind::Override => ReferenceKindInt::Override,
        ReferenceKind::Include => ReferenceKindInt::Include,
        ReferenceKind::MacroUsage => ReferenceKindInt::MacroUsage,
        ReferenceKind::Unknown => ReferenceKindInt::Usage,
    }
}

/// model::DefinitionKind -> the on-disk symbol.definition_kind int.
fn translate_definition_kind(kind: DefinitionKind) -> i32 {
    if kind == DefinitionKind::Explicit {
        definition_kind::EXPLICIT
    } else {
        definition_kind::IMPLICIT
    }
}

/// Resolves a batch-local index into one of the per-kind real-id arrays;
/// kNoLocal (and, defensively, out-of-range) resolves to 0.
fn resolve_local(arr: &[i32], id: LocalId) -> i32 {
    if id != K_NO_LOCAL && (id as usize) < arr.len() {
        arr[id as usize]
    } else {
        0
    }
}

/// Comma-joined id list for the `WHERE id IN (...)` correction statements.
/// Sorted for determinism (the C++ iterates an unordered_set, whose order is
/// unspecified; the SQL result is identical for any order).
fn join_ids(ids: &HashSet<i32>) -> String {
    let mut sorted: Vec<i32> = ids.iter().copied().collect();
    sorted.sort_unstable();
    sorted
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Builds C-owned name elements, keeping the backing CStrings alive for the
/// duration of the writer call (see tests/srctrldb_writer.rs `NameHolder`).
struct FfiName {
    _backing: Vec<CString>,
    elements: Vec<SymhowNameElement>,
}

impl FfiName {
    fn new(name: &Name) -> FfiName {
        let mut backing = Vec::new();
        let mut elements = Vec::new();
        for e in &name.elements {
            let p = CString::new(e.prefix.as_str()).unwrap_or_default();
            let n = CString::new(e.name.as_str()).unwrap_or_default();
            let po = CString::new(e.postfix.as_str()).unwrap_or_default();
            elements.push(SymhowNameElement {
                prefix: p.as_ptr(),
                name: n.as_ptr(),
                postfix: po.as_ptr(),
            });
            backing.push(p);
            backing.push(n);
            backing.push(po);
        }
        FfiName { _backing: backing, elements }
    }
}

// --- SQL kept byte-for-byte identical to the C++ ----------------------------

const K_SYMHOW_SYMBOL_DEF_DDL: &str = concat!(
    "CREATE TABLE IF NOT EXISTS symhow_symbol_def(",
    "node_id INTEGER NOT NULL, ",
    "file_node_id INTEGER NOT NULL, ",
    "PRIMARY KEY(node_id, file_node_id)",
    ") WITHOUT ROWID;"
);

const K_INSERT_TOUCHED_SQL: &str = "INSERT OR IGNORE INTO symhow_touched(node_id) VALUES(?1)";

const K_TOUCHED_CLOSURE_SQL: &str = concat!(
    "INSERT OR IGNORE INTO symhow_touched(node_id) SELECT e.source_node_id FROM edge e ",
    "JOIN symhow_touched t ON t.node_id = e.target_node_id WHERE e.type = 1;"
);

const K_TOUCHED_UNSOLVED_SQL: &str = concat!(
    "INSERT OR IGNORE INTO symhow_touched(node_id) SELECT id FROM node ",
    "WHERE serialized_name = char(9) || 'munsolved symbol' || char(9) || 's' || char(9) || 'p';"
);

const K_UPDATE_NODE_TYPE_SQL: &str = concat!(
    "UPDATE node SET type = ?1 ",
    "WHERE id = ?2 AND (",
    "  (CASE type WHEN ?3 THEN 0 WHEN ?4 THEN 1 ELSE 2 END) < ?5 ",
    "  OR ((CASE type WHEN ?3 THEN 0 WHEN ?4 THEN 1 ELSE 2 END) = ?5 AND type < ?1)",
    ")"
);

const K_UPSERT_SYMBOL_SQL: &str = concat!(
    "INSERT INTO symbol(id, definition_kind) VALUES(?1, ?2) ",
    "ON CONFLICT(id) DO UPDATE SET definition_kind = max(definition_kind, excluded.definition_kind)"
);

const K_INSERT_SYMBOL_DEF_SQL: &str =
    "INSERT OR IGNORE INTO symhow_symbol_def(node_id, file_node_id) VALUES(?1, ?2)";

const K_UPSERT_TU_SQL: &str = concat!(
    "INSERT INTO symhow_tu(path, args_hash, file_node_id, status, entry_count) VALUES(?1, ?2, ?3, ?4, ?5) ",
    "ON CONFLICT(path) DO UPDATE SET args_hash = excluded.args_hash, file_node_id = excluded.file_node_id, ",
    "status = excluded.status, entry_count = excluded.entry_count"
);

const K_INDEX_STATEMENTS: [&str; 9] = [
    "CREATE INDEX IF NOT EXISTS symhow_edge_source_idx ON edge(source_node_id);",
    "CREATE INDEX IF NOT EXISTS symhow_edge_target_idx ON edge(target_node_id);",
    "CREATE INDEX IF NOT EXISTS symhow_edge_type_idx ON edge(type);",
    "CREATE INDEX IF NOT EXISTS symhow_node_type_idx ON node(type);",
    "CREATE INDEX IF NOT EXISTS symhow_srcloc_file_idx ON source_location(file_node_id);",
    "CREATE INDEX IF NOT EXISTS symhow_occurrence_elem_idx ON occurrence(element_id);",
    "CREATE INDEX IF NOT EXISTS symhow_occurrence_loc_idx ON occurrence(source_location_id);",
    "CREATE INDEX IF NOT EXISTS symhow_file_path_idx ON file(path);",
    "CREATE INDEX IF NOT EXISTS symhow_symbol_defkind_idx ON symbol(definition_kind);",
];

/// One symhow_tu row to upsert once its PATH's LAST CDB entry (in CDB order)
/// has been consumed since the last checkpoint -- see checkpoint()'s doc
/// comment. `args_hash`/`entry_count` are the PATH's combined values (see
/// compile_db::path_args_hashes()), and `status` is the worst outcome across
/// every one of the path's entries ("failed" > "partial" > "ok").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTuRow {
    /// CompileEntry::file (post-remap spelling).
    pub path: String,
    /// path_args_hashes()[path].combined_hash.
    pub args_hash: String,
    /// "ok" / "partial" / "failed" -- worst-so-far across this path's entries.
    pub status: String,
    /// path_args_hashes()[path].entry_count.
    pub entry_count: u32,
}

impl Default for PendingTuRow {
    fn default() -> Self {
        PendingTuRow {
            path: String::new(),
            args_hash: String::new(),
            status: String::new(),
            entry_count: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BufferedKindDef {
    kind: SymbolKind,
    definition: DefinitionKind,
}

/// The write-path backend. Mirrors `symhow::index::SrctrldbBackend`.
pub struct SrctrldbBackend {
    writer: Option<Writer>,
    no_vacuum: bool,
    verbose: bool,

    /// Second, independent connection to the same file, used ONLY while the
    /// writer's own transaction is closed (between commit_transaction() and
    /// the next begin_transaction()/close()) -- opened in begin(), right
    /// after the writer created the schema; lives for the whole session.
    side_db: Option<WritableDb>,

    /// path -> real sqlite node id (shared by FileFact rows and
    /// SymbolKind::File "file symbols" used as INCLUDE/MACRO_USAGE edge
    /// endpoints -- both resolve to the SAME node).
    file_node_id_by_path: HashMap<String, i32>,

    /// Non-indexed file ids seen since the last flush -- applied (UPDATE file
    /// SET indexed=0,complete=0; DELETE FROM filecontent/fingerprint) by
    /// flush_pending() on EVERY checkpoint/finish, not just the final one.
    /// Cleared after each flush (delta only); any_non_indexed_files (never
    /// cleared) is what finish() checks to decide whether VACUUM is needed.
    non_indexed_file_ids: HashSet<i32>,
    any_non_indexed_files: bool,

    /// Parsed-buffer hashes, carried across checkpoints to detect a header
    /// observed with different bytes by two TUs in this session. A conflict
    /// permanently disables that file's skip optimization for this session.
    file_fingerprints: HashMap<i32, String>,
    pending_file_fingerprints: HashSet<i32>,
    has_existing_files: bool,
    has_existing_fingerprints: bool,

    /// paths::normalize(path) -> real node id, kept in lockstep with
    /// file_node_id_by_path -- lets the symhow_tu upsert resolve a
    /// PendingTuRow's path to a file_node_id in memory instead of a
    /// `SELECT id, path FROM file` scan on every checkpoint.
    normalized_file_node_id: HashMap<String, i32>,

    /// Set when this pass materialized at least one reference to the vendored
    /// writer's shared "unsolved symbol" node (which never passes through
    /// by_key and would otherwise be missing from symhow_touched).
    saw_unsolved_reference: bool,

    /// Whether symhow_touched has already been (re)created THIS session: the
    /// very first flush DROPs and recreates it; every later flush in the SAME
    /// session only INSERTs the delta.
    symhow_touched_initialized: bool,

    /// Node ids touched (created or looked up) since the last flush --
    /// drained into symhow_touched by flush_pending() and cleared after.
    touched_since_flush: HashSet<i32>,

    /// SymbolKey -> real node id.
    by_key: HashMap<String, i32>,
    /// serialized_name -> SymbolKey (first owner).
    by_name: HashMap<String, String>,

    pending_kind_def: HashMap<i32, BufferedKindDef>,

    /// (node id, file node id) pairs buffered across every consume() call,
    /// populated from SymbolFact::decl_files (drained into symhow_symbol_def
    /// by flush_pending(); the eventual INSERT OR IGNORE dedups).
    pending_symbol_def: HashSet<(i32, i32)>,

    local_symbol_id_by_name: HashMap<String, i32>,

    /// (ctx, target, kind) -> reference id.
    ref_by_triple: HashMap<(i32, i32, i32), i32>,

    summary: IndexSummary,
}

impl SrctrldbBackend {
    pub fn new() -> SrctrldbBackend {
        SrctrldbBackend {
            writer: None,
            no_vacuum: false,
            verbose: false,
            side_db: None,
            file_node_id_by_path: HashMap::new(),
            non_indexed_file_ids: HashSet::new(),
            any_non_indexed_files: false,
            file_fingerprints: HashMap::new(),
            pending_file_fingerprints: HashSet::new(),
            has_existing_files: false,
            has_existing_fingerprints: false,
            normalized_file_node_id: HashMap::new(),
            saw_unsolved_reference: false,
            symhow_touched_initialized: false,
            touched_since_flush: HashSet::new(),
            by_key: HashMap::new(),
            by_name: HashMap::new(),
            pending_kind_def: HashMap::new(),
            pending_symbol_def: HashSet::new(),
            local_symbol_id_by_name: HashMap::new(),
            ref_by_triple: HashMap::new(),
            summary: IndexSummary::default(),
        }
    }

    pub fn set_no_vacuum(&mut self, value: bool) {
        self.no_vacuum = value;
    }

    pub fn set_verbose(&mut self, value: bool) {
        self.verbose = value;
    }

    /// Starts the writer's transaction that consume() calls write into.
    /// Must be called exactly once, after begin() and before the first
    /// consume()/checkpoint() call.
    pub fn start_transaction(&mut self) -> Result<()> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(Error::new("startTransaction() called before begin()"));
        };
        if !writer.begin_transaction() {
            return Err(Error::new(writer.last_error()));
        }
        Ok(())
    }

    /// Crash-safe checkpoint: (a) commits the writer's currently-open
    /// transaction; (b) via a SEPARATE WritableDb connection to the same
    /// file, in one transaction, flushes every buffered write (upgrade-only
    /// node.type UPDATE / definition_kind UPSERT, symhow_symbol_def inserts,
    /// the symhow_touched delta, file fingerprints, and `tu_rows` as
    /// symhow_tu upserts); (c) reopens a new writer transaction for
    /// subsequent consume() calls. The buffers this drains are cleared after
    /// the flush; the long-lived dedup caches (by_key, file_node_id_by_path,
    /// ...) are untouched -- they must persist for the whole session.
    pub fn checkpoint(&mut self, tu_rows: &[PendingTuRow]) -> Result<()> {
        self.commit_and_flush(tu_rows)?;
        self.start_transaction()
    }

    /// Interrupt path: identical to checkpoint()'s (a)+(b), but then CLOSES
    /// the writer cleanly instead of reopening a transaction, and does NOT
    /// create reader indices / run ANALYZE/VACUUM / clear
    /// symhow_index_in_progress -- finish() (via a later `symhow update`)
    /// does all of that. Safe to call at most once, instead of finish().
    pub fn finish_interrupted(&mut self, tu_rows: &[PendingTuRow]) -> Result<()> {
        self.commit_and_flush(tu_rows)?;
        let Some(writer) = self.writer.as_ref() else {
            return Err(Error::new("finishInterrupted() called before begin()"));
        };
        if !writer.close() {
            return Err(Error::new(writer.last_error()));
        }
        self.writer = None;
        self.side_db = None;
        // Deliberately: no reader indices, no ANALYZE/VACUUM, and
        // symhow_index_in_progress is left set.
        Ok(())
    }

    /// Shared by checkpoint()/finish_interrupted()/finish(): commits the
    /// writer's transaction, then flushes every pending buffered write (+
    /// `tu_rows`) via side_db in its own transaction. Does not touch the
    /// writer's transaction state afterward -- callers decide whether to
    /// reopen (checkpoint()) or close (finish_interrupted()/finish()).
    fn commit_and_flush(&mut self, tu_rows: &[PendingTuRow]) -> Result<()> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(Error::new("checkpoint()/finish() called before begin()"));
        };
        if !writer.commit_transaction() {
            return Err(Error::new(writer.last_error()));
        }

        let mut side_db = match self.side_db.take() {
            Some(db) => db,
            None => return Err(Error::new("checkpoint()/finish() called before begin()")),
        };
        let result = (|| -> Result<()> {
            side_db.begin()?;
            match self.flush_pending(&side_db, tu_rows) {
                Ok(()) => side_db.commit(),
                Err(e) => {
                    // The C++ leaves the side transaction open on error; roll
                    // back here so a later checkpoint can still begin.
                    let _ = side_db.rollback();
                    Err(e)
                }
            }
        })();
        self.side_db = Some(side_db);
        result
    }

    /// flushPending: every symhow-private side-table write, inside the
    /// caller's transaction on `db`. Drains every pending buffer.
    fn flush_pending(&mut self, db: &WritableDb, tu_rows: &[PendingTuRow]) -> Result<()> {
        db.exec(K_SYMHOW_FILE_FINGERPRINT_DDL)?;

        // Non-indexed files touched since the last flush: the vendored writer
        // always creates a `file` row as indexed=1/complete=1, so every file
        // this pass recorded but marked non-indexed needs this correction.
        // Applied on EVERY flush (not just finish()'s final one).
        if !self.non_indexed_file_ids.is_empty() {
            let id_list = join_ids(&self.non_indexed_file_ids);
            db.exec(&format!("UPDATE file SET indexed = 0, complete = 0 WHERE id IN ({});", id_list))?;
            db.exec(&format!("DELETE FROM filecontent WHERE id IN ({});", id_list))?;
            db.exec(&format!(
                "DELETE FROM symhow_file_fingerprint WHERE file_node_id IN ({});",
                id_list
            ))?;
            self.non_indexed_file_ids.clear();
        }

        // symhow-private side tables. IF NOT EXISTS: cheap and idempotent, so
        // safe to redo on every checkpoint.
        db.exec(K_SYMHOW_SYMBOL_DEF_DDL)?;
        db.exec(K_SYMHOW_TU_DDL)?;

        // Persist only this checkpoint's delta, from the bytes libclang
        // parsed, never by re-reading disk after parsing.
        let mut pending_fingerprints: Vec<i32> = self.pending_file_fingerprints.iter().copied().collect();
        pending_fingerprints.sort_unstable();
        for id in pending_fingerprints {
            let hash = self.file_fingerprints.get(&id).cloned().unwrap_or_default();
            db.exec_bound("DELETE FROM symhow_file_fingerprint WHERE file_node_id=?1", params![id])?;
            if !hash.is_empty() {
                db.exec_bound(
                    "INSERT INTO symhow_file_fingerprint(file_node_id, sha256) SELECT id, ?2 FROM file WHERE id=?1 AND indexed=1 AND complete=1",
                    params![id, hash],
                )?;
            }
        }
        self.pending_file_fingerprints.clear();

        // symhow_touched: every node id THIS indexing pass created or looked
        // up. DROP+recreate happens exactly ONCE per session (the first
        // flush), not on every checkpoint: symhow_touched must describe the
        // WHOLE session's output, not just the latest chunk's.
        if !self.symhow_touched_initialized {
            db.exec("DROP TABLE IF EXISTS symhow_touched;")?;
            db.exec("CREATE TABLE symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;")?;
            self.symhow_touched_initialized = true;
        }
        {
            let mut touched: Vec<i32> = self.touched_since_flush.iter().copied().collect();
            touched.sort_unstable();
            for node_id in touched {
                if node_id == 0 {
                    continue;
                }
                db.exec_bound(K_INSERT_TOUCHED_SQL, params![node_id])?;
            }
            self.touched_since_flush.clear();

            // Ancestor closure over MEMBER edges (type = 1): the vendored
            // writer materializes every INTERMEDIATE name element of a
            // NameHierarchy as its own node but only hands back the LEAF's
            // id, so a purely structural scope node can be created by this
            // pass without ever passing through by_key. Rerun on every flush
            // (idempotent) since a later chunk can add new leaves under an
            // already-touched ancestor.
            loop {
                db.exec(K_TOUCHED_CLOSURE_SQL)?;
                if db.changes() <= 0 {
                    break;
                }
            }

            if self.saw_unsolved_reference {
                // The writer's shared "unsolved symbol" node is created
                // inside the vendored writer and never passes through
                // by_key. Matched on its exact serialized name -- char(9) is
                // the '\t' separator NameHierarchy serializes with. Only
                // when this pass actually produced an unsolved reference.
                db.exec(K_TOUCHED_UNSOLVED_SQL)?;
            }
        }

        // --- node.type / symbol.definition_kind: upgrade-only writes -----
        //
        // Rank the STORED node-kind ints (not translate_symbol_kind()'s
        // model enum -- that maps BOTH Unknown and Type to TYPE, so ranking
        // against it made the ladder degenerate: every freshly-created node,
        // which the vendored writer creates as NodeKind::UNKNOWN, fell into
        // the CASE's ELSE arm and was never upgraded). rank(UNKNOWN) = 0
        // matches kindRank(SymbolKind::Unknown) = 0, and rank(TYPE) = 1
        // matches kindRank(SymbolKind::Type) = 1.
        let unknown_node_kind_int = node_kind::UNKNOWN;
        let type_node_kind_int = node_kind::TYPE;

        // Strictly-upgrading, and fully order-INDEPENDENT (including between
        // equal-ranked kinds): a stored kind is only replaced by one that's
        // STRICTLY greater under kindOrder(kind) = (rank(kind),
        // nodeKindInt(kind)) -- rank compared first, and on a RANK TIE, the
        // stored node-kind int itself (`type`) is compared against the new
        // kind's int, with the new kind winning only if it's numerically
        // greater. UNKNOWN's int is strictly less than every other NodeKind
        // int, so a rank-0-vs-rank-0 tie against a virgin UNKNOWN node still
        // upgrades it -- the final node.type is a pure function of the SET of
        // observed kinds, independent of TU order or session boundaries.
        let pending_kind_def: Vec<(i32, BufferedKindDef)> = self.pending_kind_def.drain().collect();
        for (node_id, kd) in pending_kind_def {
            // No "new_rank > 0" skip here: even a pending Unknown kind (rank
            // 0) must still be allowed to upgrade a virgin UNKNOWN node to
            // TYPE -- the WHERE clause's own rank guard is what keeps a real
            // kind from ever being downgraded.
            let new_node_kind_int = translate_symbol_kind(kd.kind);
            let new_rank = kind_rank(kd.kind);
            db.exec_bound(
                K_UPDATE_NODE_TYPE_SQL,
                params![new_node_kind_int, node_id, unknown_node_kind_int, type_node_kind_int, new_rank],
            )?;

            if kd.definition != DefinitionKind::None {
                let dk_int = translate_definition_kind(kd.definition);
                db.exec_bound(K_UPSERT_SYMBOL_SQL, params![node_id, dk_int])?;
            }
        }

        let pending_symbol_def: Vec<(i32, i32)> = self.pending_symbol_def.drain().collect();
        for (node_id, file_id) in pending_symbol_def {
            db.exec_bound(K_INSERT_SYMBOL_DEF_SQL, params![node_id, file_id])?;
        }

        // --- symhow_tu: one upsert per PATH whose LAST entry was consumed
        // since the last flush. `tu_rows` only ever contains one row per
        // path that actually completed (the driver only pushes a row once a
        // path's last CDB entry, in CDB order, has been consumed), so an
        // interrupted run that stopped mid-path never reaches this UPSERT
        // for that path at all. Still idempotent since a `symhow update`
        // re-index of an already-indexed path legitimately reaches it again.
        for row in tu_rows {
            let normalized_path = paths::normalize(&row.path);
            let file_node_id = self.normalized_file_node_id.get(&normalized_path).copied();
            db.exec_bound(
                K_UPSERT_TU_SQL,
                params![row.path, row.args_hash, file_node_id, row.status, row.entry_count as i64],
            )?;
        }

        Ok(())
    }

    // --- node ensure/lookup helpers ------------------------------------------

    fn ensure_file_node_by_path(&mut self, path: &str, language: &str, indexed: bool) -> i32 {
        let mut id = 0;
        let mut cache_hit = false;
        if let Some(&existing) = self.file_node_id_by_path.get(path) {
            id = existing;
            cache_hit = true;
        }
        if !cache_hit {
            id = match self.writer.as_ref() {
                Some(writer) => writer.record_file(path),
                None => 0,
            };
            if id != 0 {
                if let Some(writer) = self.writer.as_ref() {
                    writer.record_file_language(id, if language.is_empty() { "cpp" } else { language });
                }
                self.summary.files += 1;
            }
            self.file_node_id_by_path.insert(path.to_string(), id);
            if id != 0 {
                let normalized = paths::normalize(path);
                self.normalized_file_node_id.insert(normalized, id);
            }
        }
        if id != 0 {
            self.touched_since_flush.insert(id);
        }
        if !indexed && id != 0 {
            self.non_indexed_file_ids.insert(id);
            self.any_non_indexed_files = true;
        }
        id
    }

    fn ensure_file_node(&mut self, fact: &FileFact) -> Result<i32> {
        let id = self.ensure_file_node_by_path(&fact.path, &fact.language, fact.indexed);
        if id != 0 && fact.indexed {
            match self.file_fingerprints.get(&id) {
                None => {
                    // Newly materialized (or first-seen-this-session) file.
                    self.file_fingerprints.insert(id, fact.content_hash.clone());
                    if self.has_existing_files {
                        // A surviving shared header may still have facts from
                        // TUs NOT re-indexed this pass. Never certify new
                        // bytes over old/mixed facts. Only a newly
                        // materialized file, or a surviving file with an
                        // identical prior hash, can get a trusted fingerprint.
                        let sql = if self.has_existing_fingerprints {
                            "SELECT fp.sha256 FROM file f LEFT JOIN symhow_file_fingerprint fp ON fp.file_node_id=f.id WHERE f.id=?1"
                        } else {
                            "SELECT NULL FROM file WHERE id=?1"
                        };
                        let mut clear = false;
                        if let Some(db) = self.side_db.as_ref() {
                            db.query_bound(sql, params![id], |row| {
                                let previous: Option<String> = row.get(0).ok();
                                if previous.is_none() || previous.as_deref() != Some(fact.content_hash.as_str()) {
                                    clear = true;
                                }
                                Ok(())
                            })?;
                        }
                        if clear {
                            self.file_fingerprints.insert(id, String::new());
                        }
                    }
                    self.pending_file_fingerprints.insert(id);
                }
                Some(stored) => {
                    if stored != &fact.content_hash {
                        // Mixed/unknown parsed bytes: never skip.
                        self.file_fingerprints.insert(id, String::new());
                        self.pending_file_fingerprints.insert(id);
                    }
                }
            }
        }
        Ok(id)
    }

    /// Resolves batch.stats.tu_file to a real file node id for range-less
    /// error attachment: batch.stats.tu_file is the CDB entry's `file` field
    /// (already normalized by IndexDriver), while every FileFact.path comes
    /// from clang's own clang_getFileName() and can still differ textually.
    /// Look for a batch FileFact whose normalized path matches first, and
    /// only fall back to minting a new one.
    fn resolve_tu_file_real(&mut self, batch: &TuFactBatch, file_real: &[i32]) -> i32 {
        let normalized_tu_file = paths::normalize(&batch.stats.tu_file);
        for i in 0..batch.files.len() {
            if paths::normalize(&batch.files[i].path) == normalized_tu_file {
                return file_real[i];
            }
        }
        self.ensure_file_node_by_path(&normalized_tu_file, "cpp", true)
    }

    /// Buffers the best-so-far (kind, definition) for `node_id`, with the
    /// deterministic, order-INDEPENDENT tie-break on equal rank: the higher
    /// stored-node-kind int wins (the exact same total order the
    /// K_UPDATE_NODE_TYPE_SQL guard uses).
    fn buffer_kind_def(&mut self, node_id: i32, kind: SymbolKind, definition: DefinitionKind) {
        let buffered = self.pending_kind_def.entry(node_id).or_default();
        let new_rank = kind_rank(kind);
        let cur_rank = kind_rank(buffered.kind);
        let mut take_kind = new_rank > cur_rank;
        if !take_kind && new_rank == cur_rank {
            let new_kind_int = translate_symbol_kind(kind);
            let cur_kind_int = translate_symbol_kind(buffered.kind);
            take_kind = new_kind_int > cur_kind_int;
        }
        if take_kind {
            buffered.kind = kind;
        }
        if (definition as u8) > (buffered.definition as u8) {
            buffered.definition = definition;
        }
    }

    fn ensure_symbol_node(&mut self, fact: &SymbolFact) -> i32 {
        if fact.kind == SymbolKind::File {
            // File "symbols" reuse the exact same node id as the
            // corresponding FileFact's own node (there is no separate
            // sourcetrail SymbolKind for files). fact.key IS the file's
            // absolute path by construction; the FileFact for this path is
            // always recorded before AstWalker asks for the corresponding
            // File-kind symbol, so this should always be a cache hit.
            if let Some(&id) = self.by_key.get(&fact.key) {
                self.touched_since_flush.insert(id);
                return id;
            }
            // Already inserts into touched_since_flush.
            let id = self.ensure_file_node_by_path(&fact.key, "cpp", true);
            self.by_key.insert(fact.key.clone(), id);
            return id;
        }

        if let Some(&id) = self.by_key.get(&fact.key) {
            self.buffer_kind_def(id, fact.kind, fact.definition);
            self.touched_since_flush.insert(id);
            return id;
        }

        let ffi_name = FfiName::new(&fact.name);
        let serialized = serialize_name_hierarchy(&fact.name.delimiter, &ffi_name.elements);

        if let Some(first_key) = self.by_name.get(&serialized).cloned() {
            // Residual collision: two distinct SymbolKeys serialize to the
            // same display name. The schema itself dedups nodes by
            // serialized_name, so the vendored writer would silently reuse
            // the same node anyway; make that explicit, count it, and warn.
            let id = *self
                .by_key
                .get(&first_key)
                .expect("by_name owner missing from by_key");
            self.by_key.insert(fact.key.clone(), id);
            self.buffer_kind_def(id, fact.kind, fact.definition);
            self.summary.name_collisions += 1;
            if self.verbose {
                eprintln!(
                    "symhow index: warning: symbol name collision -- keys '{}' and '{}' both serialize to '{}'; merged into node {}",
                    first_key,
                    fact.key,
                    serialized,
                    id
                );
            }
            self.touched_since_flush.insert(id);
            return id;
        }

        let id = match self.writer.as_ref() {
            Some(writer) => writer.record_symbol(&fact.name.delimiter, &ffi_name.elements),
            None => 0,
        };
        self.by_key.insert(fact.key.clone(), id);
        self.by_name.insert(serialized, fact.key.clone());
        self.buffer_kind_def(id, fact.kind, fact.definition);
        self.summary.symbols += 1;
        self.touched_since_flush.insert(id);
        id
    }

    fn ensure_local_symbol_node(&mut self, name: &str) -> i32 {
        if let Some(&id) = self.local_symbol_id_by_name.get(name) {
            return id;
        }
        let id = match self.writer.as_ref() {
            Some(writer) => writer.record_local_symbol(name),
            None => 0,
        };
        self.local_symbol_id_by_name.insert(name.to_string(), id);
        id
    }

    fn ensure_reference(&mut self, ctx_real: i32, target_real: i32, kind: ReferenceKind) -> i32 {
        let key = (ctx_real, target_real, kind as i32);
        if let Some(&id) = self.ref_by_triple.get(&key) {
            return id;
        }
        let id = match self.writer.as_ref() {
            Some(writer) => writer.record_reference(ctx_real, target_real, translate_reference_kind(kind)),
            None => 0,
        };
        self.ref_by_triple.insert(key, id);
        self.summary.references += 1;
        id
    }
}

impl Default for SrctrldbBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBackend for SrctrldbBackend {
    /// Opens the output db and its schema (SourcetrailDBWriter::open creates
    /// it), but does NOT start the writer's first transaction -- that gap is
    /// where IndexDriver writes provenance (+ symhow_index_in_progress) via
    /// its own tiny WritableDb transaction, BEFORE any TU data is written.
    fn begin(&mut self, db_path: &str) -> Result<()> {
        let writer = Writer::new();
        if !writer.open(db_path) {
            return Err(Error::new(writer.last_error()));
        }
        self.writer = Some(writer);

        let side_db = WritableDb::open(db_path, /* foreign_keys= */ false)?;
        let mut has_existing_files = false;
        side_db.query("SELECT 1 FROM file LIMIT 1", |_row| {
            has_existing_files = true;
            Ok(())
        })?;
        self.has_existing_files = has_existing_files;
        let mut has_existing_fingerprints = false;
        side_db.query(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='symhow_file_fingerprint'",
            |_row| {
                has_existing_fingerprints = true;
                Ok(())
            },
        )?;
        self.has_existing_fingerprints = has_existing_fingerprints;
        self.side_db = Some(side_db);

        Ok(())
    }

    /// Writer batching order: files -> symbols (with declFiles ->
    /// pending_symbol_def) -> localSymbols -> references (solved now,
    /// unsolved deferred to their occurrence) -> occurrences -> errors.
    fn consume(&mut self, batch: &TuFactBatch) -> Result<()> {
        if self.writer.is_none() {
            return Err(Error::new("consume() called before begin()"));
        }

        let mut file_real: Vec<i32> = vec![0; batch.files.len()];
        for (i, fact) in batch.files.iter().enumerate() {
            file_real[i] = self.ensure_file_node(fact)?;
        }

        let mut sym_real: Vec<i32> = vec![0; batch.symbols.len()];
        for (i, fact) in batch.symbols.iter().enumerate() {
            sym_real[i] = self.ensure_symbol_node(fact);
            let sym_id = sym_real[i];
            if sym_id != 0 {
                for decl_file in &fact.decl_files {
                    let file_id = resolve_local(&file_real, *decl_file);
                    if file_id != 0 {
                        self.pending_symbol_def.insert((sym_id, file_id));
                    }
                }
            }
        }

        let mut local_real: Vec<i32> = vec![0; batch.local_symbols.len()];
        for (i, name) in batch.local_symbols.iter().enumerate() {
            local_real[i] = self.ensure_local_symbol_node(name);
        }

        struct RefState {
            solved: bool,
            real_id: i32,
            kind: ReferenceKind,
            ctx_real: i32,
        }
        let mut ref_states: Vec<RefState> = Vec::with_capacity(batch.references.len());
        for rf in &batch.references {
            let ctx_real = resolve_local(&sym_real, rf.context);
            if rf.target != K_NO_LOCAL {
                let tgt_real = resolve_local(&sym_real, rf.target);
                let real_id = self.ensure_reference(ctx_real, tgt_real, rf.kind);
                ref_states.push(RefState { solved: true, real_id, kind: rf.kind, ctx_real });
            } else {
                // Unsolved: record_reference_to_unsolved needs a location up
                // front, so materialization is deferred to the matching
                // occurrence below (AstWalker always pairs unsolved_reference()
                // with exactly one occurrence() call).
                ref_states.push(RefState { solved: false, real_id: 0, kind: rf.kind, ctx_real });
            }
        }

        for occ in &batch.occurrences {
            let sr = SymhowSourceRange {
                file_id: resolve_local(&file_real, occ.range.file),
                start_line: occ.range.start_line as i32,
                start_column: occ.range.start_col as i32,
                end_line: occ.range.end_line as i32,
                end_column: occ.range.end_col as i32,
            };
            let writer = self.writer.as_ref().expect("checked above");
            match occ.cls {
                ElementClass::Symbol => {
                    let id = resolve_local(&sym_real, occ.element);
                    match occ.kind {
                        LocationKind::Token => {
                            writer.record_symbol_location(id, sr);
                        }
                        LocationKind::Scope => {
                            writer.record_symbol_scope_location(id, sr);
                        }
                        LocationKind::Signature => {
                            writer.record_symbol_signature_location(id, sr);
                        }
                        LocationKind::Qualifier => {
                            writer.record_qualifier_location(id, sr);
                        }
                        _ => {}
                    }
                }
                ElementClass::LocalSymbol => {
                    let id = resolve_local(&local_real, occ.element);
                    writer.record_local_symbol_location(id, sr);
                }
                ElementClass::Reference => {
                    if (occ.element as usize) >= ref_states.len() {
                        continue;
                    }
                    let state = &mut ref_states[occ.element as usize];
                    if state.solved {
                        writer.record_reference_location(state.real_id, sr);
                    } else {
                        state.real_id = writer.record_reference_to_unsolved(
                            state.ctx_real,
                            translate_reference_kind(state.kind),
                            sr,
                        );
                        state.solved = true;
                        self.saw_unsolved_reference = true;
                        self.summary.references += 1;
                    }
                }
            }
        }

        let mut tu_file_real = 0;
        for e in &batch.errors {
            let mut sr = SymhowSourceRange {
                file_id: 0,
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
            };
            if e.has_range {
                sr = SymhowSourceRange {
                    file_id: resolve_local(&file_real, e.range.file),
                    start_line: e.range.start_line as i32,
                    start_column: e.range.start_col as i32,
                    end_line: e.range.end_line as i32,
                    end_column: e.range.end_col as i32,
                };
            } else if !batch.stats.tu_file.is_empty() {
                // Attach range-less errors (e.g. a sanitize()/parse failure
                // with no diagnostic location) to the TU's own main file node
                // rather than an arbitrary first file of the batch -- never 0.
                if tu_file_real == 0 {
                    tu_file_real = self.resolve_tu_file_real(batch, &file_real);
                }
                sr.file_id = tu_file_real;
            }
            let writer = self.writer.as_ref().expect("checked above");
            writer.record_error(&e.message, e.fatal, sr);
            self.summary.errors += 1;
        }

        Ok(())
    }

    /// Callers MUST call checkpoint() one last time immediately before
    /// finish(): by the time finish() runs, every buffer is already empty
    /// and every symhow_tu row already written, so finish() itself only
    /// needs to close out the writer's (by-then-empty) transaction, create
    /// reader indices, run ANALYZE/VACUUM, and clear symhow_index_in_progress.
    fn finish(&mut self, summary: &mut IndexSummary) -> Result<()> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(Error::new("finish() called before begin()"));
        };
        if !writer.commit_transaction() {
            return Err(Error::new(writer.last_error()));
        }
        if !writer.close() {
            return Err(Error::new(writer.last_error()));
        }
        self.writer = None;

        // Post-write correction + reader indices, via our own raw sqlite
        // handle -- never patch the vendored writer to do this instead.
        // Reuses side_db (already open) rather than a fresh connection.
        let result = (|| -> Result<()> {
            let Some(db) = self.side_db.as_mut() else {
                return Err(Error::new("finish() called before begin()"));
            };
            db.begin()?;

            // Any non-indexed files from THIS (final) checkpoint's own delta
            // -- flush_pending() already handled every prior checkpoint's;
            // kept for defensiveness (a caller that skips the pre-finish()
            // checkpoint entirely still gets correct behavior).
            if !self.non_indexed_file_ids.is_empty() {
                let id_list = join_ids(&self.non_indexed_file_ids);
                db.exec(&format!("UPDATE file SET indexed = 0, complete = 0 WHERE id IN ({});", id_list))?;
                db.exec(&format!("DELETE FROM filecontent WHERE id IN ({});", id_list))?;
                self.non_indexed_file_ids.clear();
            }

            delete_meta(db, "symhow_index_in_progress")?;

            for stmt in K_INDEX_STATEMENTS {
                db.exec(stmt)?;
            }

            db.commit()?;

            // ANALYZE/VACUUM are best-effort maintenance, not correctness --
            // a fully-written, already-committed database must not be treated
            // as a failed `symhow index` run just because one of these
            // fails. Downgrade each to an independent stderr warning.
            if let Err(e) = db.exec("ANALYZE;") {
                eprintln!("symhow index: warning: ANALYZE failed: {}", e.0);
            }
            if !self.no_vacuum && self.any_non_indexed_files {
                if let Err(e) = db.exec("VACUUM;") {
                    eprintln!("symhow index: warning: VACUUM failed: {}", e.0);
                }
            }
            Ok(())
        })();
        result?;

        self.side_db = None;

        summary.symbols = self.summary.symbols;
        summary.references = self.summary.references;
        summary.files = self.summary.files;
        summary.errors = self.summary.errors;
        summary.name_collisions = self.summary.name_collisions;
        Ok(())
    }
}
