//! A logical, id-independent snapshot of a Sourcetrail-schema database.
//! Mirrors `symhow::update::Snapshot`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::common::Result;
use crate::db::Database;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub nodes: BTreeMap<String, i32>,         // "serialized_name|type"
    pub edges: BTreeMap<String, i32>,        // "source|target|type"
    pub locations: BTreeMap<String, i32>,    // "path|sl|sc|el|ec|type|elementKey"
    pub symbols: BTreeMap<String, i32>,      // "serialized_name|definition_kind"
    pub files: BTreeMap<String, i32>,        // "path|indexed|complete|line_count"
    pub errors: BTreeMap<String, i32>,       // "message|fatal|filePathOrDash"
    pub local_symbols: BTreeMap<String, i32>, // "name"
    pub symbol_defs: BTreeMap<String, i32>,  // "serialized_name|file.path"
    pub tus: BTreeMap<String, i32>,          // "path|args_hash"
    pub notes: BTreeMap<String, i32>,        // "serialized_name|notes_path|brief|what|how|filled|stale|note_hash"
    pub note_problems: BTreeMap<String, i32>, // "notes_path|line|kind"
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotOptions {
    /// Drop `locations` rows whose file is a SHARED header (the target of
    /// INCLUDE edges from two or more distinct includers).
    pub ignore_shared_header_locations: bool,
}

fn join(parts: &[&str]) -> String {
    parts.join("|")
}

/// Opens `db_path` read-only and builds a `Snapshot`.
pub fn take_snapshot(db_path: &str, options: &SnapshotOptions) -> Result<Snapshot> {
    let db = Database::open(db_path, 0)?;
    let mut snap = Snapshot::default();

    // --- shared headers ---
    let mut shared_header_file_ids: HashSet<i64> = HashSet::new();
    if options.ignore_shared_header_locations {
        db.run(
            "SELECT target_node_id FROM edge WHERE type = 256 \
             GROUP BY target_node_id HAVING COUNT(DISTINCT source_node_id) >= 2",
            [],
            |row| {
                shared_header_file_ids.insert(row.get::<_, i64>(0)?);
                Ok(())
            },
        )?;
    }

    // --- nodes ---
    let mut node_serialized_name: HashMap<i64, String> = HashMap::new();
    db.run("SELECT id, type, serialized_name FROM node", [], |row| {
        let id: i64 = row.get(0)?;
        let node_type: i64 = row.get(1)?;
        let name: Option<String> = row.get(2)?;
        let name = name.unwrap_or_default();
        node_serialized_name.insert(id, name.clone());
        *snap.nodes.entry(join(&[&name, &node_type.to_string()])).or_insert(0) += 1;
        Ok(())
    })?;

    // --- edges ---
    let mut edge_element_key: HashMap<i64, String> = HashMap::new();
    db.run("SELECT id, type, source_node_id, target_node_id FROM edge", [], |row| {
        let id: i64 = row.get(0)?;
        let edge_type: i64 = row.get(1)?;
        let source_id: i64 = row.get(2)?;
        let target_id: i64 = row.get(3)?;
        let source = node_serialized_name.get(&source_id).cloned().unwrap_or_default();
        let target = node_serialized_name.get(&target_id).cloned().unwrap_or_default();
        let key = join(&[&source, &target, &edge_type.to_string()]);
        edge_element_key.insert(id, key.clone());
        *snap.edges.entry(key).or_insert(0) += 1;
        Ok(())
    })?;

    // --- local_symbol ---
    let mut local_symbol_name: HashMap<i64, String> = HashMap::new();
    db.run("SELECT id, name FROM local_symbol", [], |row| {
        let id: i64 = row.get(0)?;
        let name: Option<String> = row.get(1)?;
        let name = name.unwrap_or_default();
        local_symbol_name.insert(id, name.clone());
        *snap.local_symbols.entry(name).or_insert(0) += 1;
        Ok(())
    })?;

    // --- error ---
    let mut error_message: HashMap<i64, String> = HashMap::new();
    let mut error_fatal: HashMap<i64, String> = HashMap::new();
    db.run("SELECT id, message, fatal FROM error", [], |row| {
        let id: i64 = row.get(0)?;
        let message: Option<String> = row.get(1)?;
        let fatal: i64 = row.get(2)?;
        error_message.insert(id, message.unwrap_or_default());
        error_fatal.insert(id, fatal.to_string());
        Ok(())
    })?;

    // --- symbols ---
    db.run(
        "SELECT symbol.id, symbol.definition_kind, node.serialized_name FROM symbol JOIN node ON node.id = symbol.id",
        [],
        |row| {
            let definition_kind: i64 = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let name = name.unwrap_or_default();
            *snap.symbols.entry(join(&[&name, &definition_kind.to_string()])).or_insert(0) += 1;
            Ok(())
        },
    )?;

    // --- files ---
    db.run("SELECT path, indexed, complete, line_count FROM file", [], |row| {
        let path: Option<String> = row.get(0)?;
        let indexed: i64 = row.get(1)?;
        let complete: i64 = row.get(2)?;
        let line_count: i64 = row.get(3)?;
        let key = join(&[&path.unwrap_or_default(), &indexed.to_string(), &complete.to_string(), &line_count.to_string()]);
        *snap.files.entry(key).or_insert(0) += 1;
        Ok(())
    })?;

    // --- element_id -> first-seen file path ---
    let mut element_first_file_path: HashMap<i64, String> = HashMap::new();
    db.run(
        "SELECT o.element_id, f.path FROM occurrence o \
         JOIN source_location sl ON sl.id = o.source_location_id \
         JOIN file f ON f.id = sl.file_node_id",
        [],
        |row| {
            let element_id: i64 = row.get(0)?;
            if !element_first_file_path.contains_key(&element_id) {
                let path: Option<String> = row.get(1)?;
                element_first_file_path.insert(element_id, path.unwrap_or_default());
            }
            Ok(())
        },
    )?;

    // --- errors: (message, fatal, filePathOrDash) ---
    for (id, message) in &error_message {
        let fatal = error_fatal.get(id).cloned().unwrap_or_else(|| "0".to_string());
        let path = element_first_file_path.get(id).cloned().unwrap_or_else(|| "-".to_string());
        *snap.errors.entry(join(&[message, &fatal, &path])).or_insert(0) += 1;
    }

    // --- locations ---
    db.run(
        "SELECT o.element_id, f.path, sl.start_line, sl.start_column, sl.end_line, sl.end_column, sl.type, f.id \
         FROM occurrence o \
         JOIN source_location sl ON sl.id = o.source_location_id \
         JOIN file f ON f.id = sl.file_node_id",
        [],
        |row| {
            let element_id: i64 = row.get(0)?;
            let file_id: i64 = row.get(7)?;
            if !shared_header_file_ids.is_empty() && shared_header_file_ids.contains(&file_id) {
                return Ok(());
            }
            let path: Option<String> = row.get(1)?;
            let path = path.unwrap_or_default();
            let start_line: i64 = row.get(2)?;
            let start_col: i64 = row.get(3)?;
            let end_line: i64 = row.get(4)?;
            let end_col: i64 = row.get(5)?;
            let loc_type: i64 = row.get(6)?;

            let element_key = if let Some(name) = node_serialized_name.get(&element_id) {
                format!("n:{}", name)
            } else if let Some(key) = edge_element_key.get(&element_id) {
                format!("e:{}", key)
            } else if let Some(name) = local_symbol_name.get(&element_id) {
                format!("l:{}", name)
            } else if let Some(message) = error_message.get(&element_id) {
                format!("x:{}", message)
            } else {
                return Ok(()); // unknown element kind -- skip
            };

            let key = join(&[
                &path,
                &start_line.to_string(),
                &start_col.to_string(),
                &end_line.to_string(),
                &end_col.to_string(),
                &loc_type.to_string(),
                &element_key,
            ]);
            *snap.locations.entry(key).or_insert(0) += 1;
            Ok(())
        },
    )?;

    // --- symhow_symbol_def (skip if absent) ---
    if table_exists(&db, "symhow_symbol_def") {
        db.run(
            "SELECT n.serialized_name, f.path FROM symhow_symbol_def sd \
             JOIN node n ON n.id = sd.node_id \
             JOIN file f ON f.id = sd.file_node_id",
            [],
            |row| {
                let name: Option<String> = row.get(0)?;
                let path: Option<String> = row.get(1)?;
                let key = join(&[&name.unwrap_or_default(), &path.unwrap_or_default()]);
                *snap.symbol_defs.entry(key).or_insert(0) += 1;
                Ok(())
            },
        )?;
    }

    // --- symhow_note / symhow_note_problem (skip if absent) ---
    if table_exists(&db, "symhow_note") {
        db.run(
            "SELECT serialized_name, notes_path, brief, what, how, filled, stale, note_hash FROM symhow_note",
            [],
            |row| {
                let serialized_name: Option<String> = row.get(0)?;
                let notes_path: Option<String> = row.get(1)?;
                let brief: Option<String> = row.get(2)?;
                let what: Option<String> = row.get(3)?;
                let how: Option<String> = row.get(4)?;
                let filled: i64 = row.get(5)?;
                let stale: i64 = row.get(6)?;
                let note_hash: Option<String> = row.get(7)?;
                let key = join(&[
                    &serialized_name.unwrap_or_default(),
                    &notes_path.unwrap_or_default(),
                    &brief.unwrap_or_default(),
                    &what.unwrap_or_default(),
                    &how.unwrap_or_default(),
                    &filled.to_string(),
                    &stale.to_string(),
                    &note_hash.unwrap_or_default(),
                ]);
                *snap.notes.entry(key).or_insert(0) += 1;
                Ok(())
            },
        )?;
    }
    if table_exists(&db, "symhow_note_problem") {
        db.run("SELECT notes_path, line, kind FROM symhow_note_problem", [], |row| {
            let notes_path: Option<String> = row.get(0)?;
            let line: i64 = row.get(1)?;
            let kind: Option<String> = row.get(2)?;
            let key = join(&[&notes_path.unwrap_or_default(), &line.to_string(), &kind.unwrap_or_default()]);
            *snap.note_problems.entry(key).or_insert(0) += 1;
            Ok(())
        })?;
    }

    // --- symhow_tu (skip if absent) ---
    if table_exists(&db, "symhow_tu") {
        db.run("SELECT path, args_hash FROM symhow_tu", [], |row| {
            let path: Option<String> = row.get(0)?;
            let args_hash: Option<String> = row.get(1)?;
            let key = join(&[&path.unwrap_or_default(), &args_hash.unwrap_or_default()]);
            *snap.tus.entry(key).or_insert(0) += 1;
            Ok(())
        })?;
    }

    Ok(snap)
}

fn table_exists(db: &Database, name: &str) -> bool {
    let mut found = false;
    let _ = db.run(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
        [name],
        |_row| {
            found = true;
            Ok(())
        },
    );
    found
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotDiff {
    /// Category-prefixed, e.g. "node|::foo|4096".
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
}

fn diff_category(
    a: &BTreeMap<String, i32>,
    b: &BTreeMap<String, i32>,
    category: &str,
    only_in_a: &mut Vec<String>,
    only_in_b: &mut Vec<String>,
) {
    for (key, count_a) in a {
        match b.get(key) {
            None => only_in_a.push(format!("{}|{}", category, key)),
            Some(count_b) if count_b != count_a => {
                only_in_a.push(format!("{}|{} (x{} vs x{})", category, key, count_a, count_b));
                only_in_b.push(format!("{}|{} (x{} vs x{})", category, key, count_b, count_a));
            }
            _ => {}
        }
    }
    for (key, _count_b) in b {
        if !a.contains_key(key) {
            only_in_b.push(format!("{}|{}", category, key));
        }
    }
}

/// Diffs every category of `a` against `b`. Empty (both directions) iff `a`
/// and `b` are logically equivalent. `only`, if non-empty, restricts the
/// diff to exactly those category names.
pub fn diff_snapshots(a: &Snapshot, b: &Snapshot, only: &[String]) -> SnapshotDiff {
    let mut diff = SnapshotDiff::default();
    let only_set: HashSet<&str> = only.iter().map(|s| s.as_str()).collect();

    let categories: [(&str, &BTreeMap<String, i32>, &BTreeMap<String, i32>); 11] = [
        ("node", &a.nodes, &b.nodes),
        ("edge", &a.edges, &b.edges),
        ("location", &a.locations, &b.locations),
        ("symbol", &a.symbols, &b.symbols),
        ("file", &a.files, &b.files),
        ("error", &a.errors, &b.errors),
        ("local_symbol", &a.local_symbols, &b.local_symbols),
        ("symbol_def", &a.symbol_defs, &b.symbol_defs),
        ("tu", &a.tus, &b.tus),
        ("note", &a.notes, &b.notes),
        ("note_problem", &a.note_problems, &b.note_problems),
    ];

    for (name, a_map, b_map) in categories {
        if !only_set.is_empty() && !only_set.contains(name) {
            continue;
        }
        diff_category(a_map, b_map, name, &mut diff.only_in_a, &mut diff.only_in_b);
    }
    diff
}