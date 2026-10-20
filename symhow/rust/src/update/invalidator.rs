//! Phase 3 invalidation: computes which `file` nodes (and everything that
//! transitively depends on them) must be cleared before `symhow update`
//! re-indexes a set of changed TUs, and performs that clearing. Mirrors
//! `symhow::update::Invalidator` (see the C++ for the full algorithm
//! write-up; every SQL statement is preserved byte-for-byte).

use crate::common::{Error, Result};
use crate::db::enums::node_kind;
use crate::db::WritableDb;

#[derive(Clone, Debug, Default)]
pub struct ClearPlan {
    pub cleared_file_ids: Vec<i64>,
    pub cleared_paths: Vec<String>,
    pub seed_files: u32,
    pub include_closure_added: u32,
    pub headers_pruned: u32,
}

#[derive(Clone, Debug, Default)]
pub struct GcStats {
    pub files: i64,
    pub nodes: i64,
    pub edges: i64,
    pub locations: i64,
    pub local_symbols: i64,
    pub errors: i64,
    pub symbols_reset: i64,
    /// Always 0 since Phase 5 (kept so the `symhow update --json` GC object's
    /// shape does not change).
    pub non_indexed_files: i64,
    pub orphan_elements: i64,
}

fn create_path_temp_table(db: &WritableDb, table_name: &str) -> Result<()> {
    db.exec(&format!("DROP TABLE IF EXISTS {};", table_name))?;
    db.exec(&format!("CREATE TEMP TABLE {}(path TEXT PRIMARY KEY);", table_name))
}

fn insert_paths(db: &WritableDb, table_name: &str, paths: &[String]) -> Result<()> {
    let sql = format!("INSERT OR IGNORE INTO {}(path) VALUES(?1)", table_name);
    for p in paths {
        db.exec_bound(&sql, [p])?;
    }
    Ok(())
}

fn create_id_temp_table(db: &WritableDb, table_name: &str) -> Result<()> {
    db.exec(&format!("DROP TABLE IF EXISTS {};", table_name))?;
    db.exec(&format!("CREATE TEMP TABLE {}(id INTEGER PRIMARY KEY);", table_name))
}

fn insert_ids(db: &WritableDb, table_name: &str, ids: &[i64]) -> Result<()> {
    let sql = format!("INSERT OR IGNORE INTO {}(id) VALUES(?1)", table_name);
    for id in ids {
        db.exec_bound(&sql, [id])?;
    }
    Ok(())
}

/// Runs `sql` repeatedly until it stops adding rows; returns total rows added.
fn run_to_fixpoint(db: &WritableDb, sql: &str) -> Result<i64> {
    let mut total = 0i64;
    loop {
        db.exec(sql)?;
        let c = db.changes();
        if c <= 0 {
            break;
        }
        total += c;
    }
    Ok(total)
}

pub fn compute_clear_set(
    db: &mut WritableDb,
    changed_db_paths: &[String],
    deleted_db_paths: &[String],
    tu_paths: &[String],
) -> Result<ClearPlan> {
    let mut plan = ClearPlan::default();

    create_path_temp_table(db, "symhow_paths")?;
    insert_paths(db, "symhow_paths", changed_db_paths)?;
    insert_paths(db, "symhow_paths", deleted_db_paths)?;

    create_path_temp_table(db, "symhow_tu_paths")?;
    insert_paths(db, "symhow_tu_paths", tu_paths)?;

    create_id_temp_table(db, "symhow_clear")?;

    db.exec("INSERT OR IGNORE INTO symhow_clear SELECT id FROM file WHERE path IN (SELECT path FROM symhow_paths);")?;
    plan.seed_files = db.changes().max(0) as u32;

    // step 1: reverse INCLUDE closure -- loop until changes()==0.
    plan.include_closure_added = run_to_fixpoint(
        db,
        "INSERT OR IGNORE INTO symhow_clear SELECT e.source_node_id FROM edge e JOIN symhow_clear c ON c.id=e.target_node_id \
         WHERE e.type=256;",
    )? as u32;

    // step 2: header pruning -- loop until changes()==0.
    plan.headers_pruned = run_to_fixpoint(
        db,
        "INSERT OR IGNORE INTO symhow_clear SELECT f.id FROM file f \
         WHERE f.id NOT IN (SELECT id FROM symhow_clear) AND f.path NOT IN (SELECT path FROM symhow_tu_paths) \
         AND EXISTS (SELECT 1 FROM edge e WHERE e.type=256 AND e.target_node_id=f.id) \
         AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=256 AND e.target_node_id=f.id \
         AND e.source_node_id NOT IN (SELECT id FROM symhow_clear));",
    )? as u32;

    db.query("SELECT c.id, f.path FROM symhow_clear c JOIN file f ON f.id = c.id", |row| {
        let id: i64 = row.get(0)?;
        let path: Option<String> = row.get(1)?;
        plan.cleared_file_ids.push(id);
        plan.cleared_paths.push(path.unwrap_or_default());
        Ok(())
    })?;

    db.exec("DROP TABLE IF EXISTS symhow_clear;")?;
    db.exec("DROP TABLE IF EXISTS symhow_tu_paths;")?;
    db.exec("DROP TABLE IF EXISTS symhow_paths;")?;

    Ok(plan)
}

/// Steps 6.5-9, ALWAYS restricted to a candidate set (`cand_join`, a JOIN
/// clause on `node n`). See the C++ for the extensive rationale comments;
/// `pre_index` is kept for signature parity with the C++ (where it is
/// likewise unused in the body).
fn dead_node_and_orphan_sweep(db: &WritableDb, cand_join: &str, pre_index: bool) -> Result<GcStats> {
    let _ = pre_index;
    let mut stats = GcStats::default();

    db.exec("CREATE TABLE IF NOT EXISTS symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;")?;

    // step 6.5: a non-MEMBER edge whose SOURCE node has no occurrence of its
    // own is garbage, full stop.
    db.exec(
        "DELETE FROM element WHERE id IN (SELECT e.id FROM edge e \
         WHERE e.type<>1 AND e.source_node_id NOT IN (SELECT id FROM file) \
         AND e.source_node_id NOT IN (SELECT node_id FROM symhow_touched) \
         AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.source_node_id));",
    )?;
    stats.edges += db.changes();

    // step 7: dead candidate nodes -- loop until changes()==0.
    let dead_node_sql = format!(
        "DELETE FROM element WHERE id IN (SELECT n.id FROM node n {} \
         WHERE n.id NOT IN (SELECT id FROM file) AND n.id NOT IN (SELECT node_id FROM symhow_touched) \
         AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=n.id) \
         AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type<>1 AND (e.source_node_id=n.id OR e.target_node_id=n.id)) \
         AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.type=1 AND e.source_node_id=n.id));",
        cand_join
    );
    stats.nodes = run_to_fixpoint(db, &dead_node_sql)?;

    // step 8: local symbols / errors exist only through occurrences.
    db.exec("DELETE FROM element WHERE id IN (SELECT id FROM local_symbol) AND id NOT IN (SELECT element_id FROM occurrence);")?;
    stats.local_symbols = db.changes();

    db.exec("DELETE FROM element WHERE id IN (SELECT id FROM error) AND id NOT IN (SELECT element_id FROM occurrence);")?;
    stats.errors = db.changes();

    // (Phase 5 note preserved from the C++: there is deliberately NO
    // "delete edge-less, occurrence-less non-indexed file rows" rule here.)

    // step 9: orphan element rows.
    db.exec(
        "DELETE FROM element WHERE id NOT IN (SELECT id FROM node) AND id NOT IN (SELECT id FROM edge) \
         AND id NOT IN (SELECT id FROM local_symbol) AND id NOT IN (SELECT id FROM error);",
    )?;
    stats.orphan_elements = db.changes();

    // Dangling symhow_symbol_def / symhow_tu cleanup.
    db.exec("DELETE FROM symhow_symbol_def WHERE node_id NOT IN (SELECT id FROM node) OR file_node_id NOT IN (SELECT id FROM file);")?;
    db.exec("UPDATE symhow_tu SET file_node_id = NULL WHERE file_node_id IS NOT NULL AND file_node_id NOT IN (SELECT id FROM file);")?;

    // Orphan source_location rows.
    db.exec("DELETE FROM source_location WHERE id NOT IN (SELECT source_location_id FROM occurrence);")?;
    stats.locations += db.changes();

    Ok(stats)
}

fn invalidate_inner(db: &mut WritableDb, plan: &ClearPlan) -> Result<GcStats> {
    let mut stats = GcStats::default();

    create_id_temp_table(db, "symhow_clear")?;
    insert_ids(db, "symhow_clear", &plan.cleared_file_ids)?;

    // Empty the previous pass's symhow_touched.
    db.exec("DROP TABLE IF EXISTS symhow_touched;")?;
    db.exec("CREATE TABLE symhow_touched(node_id INTEGER PRIMARY KEY) WITHOUT ROWID;")?;

    create_path_temp_table(db, "symhow_paths")?;
    insert_paths(db, "symhow_paths", &plan.cleared_paths)?;

    // step 3: candidates.
    db.exec("DROP TABLE IF EXISTS symhow_cand;")?;
    db.exec("CREATE TEMP TABLE symhow_cand(id INTEGER PRIMARY KEY);")?;
    db.exec(
        "INSERT OR IGNORE INTO symhow_cand SELECT o.element_id FROM occurrence o \
         JOIN source_location l ON l.id=o.source_location_id WHERE l.file_node_id IN (SELECT id FROM symhow_clear);",
    )?;
    db.exec(
        "INSERT OR IGNORE INTO symhow_cand SELECT source_node_id FROM edge WHERE id IN (SELECT id FROM symhow_cand) \
         OR source_node_id IN (SELECT id FROM symhow_clear) OR target_node_id IN (SELECT id FROM symhow_clear);",
    )?;
    db.exec(
        "INSERT OR IGNORE INTO symhow_cand SELECT target_node_id FROM edge WHERE id IN (SELECT id FROM symhow_cand) \
         OR source_node_id IN (SELECT id FROM symhow_clear) OR target_node_id IN (SELECT id FROM symhow_clear);",
    )?;
    run_to_fixpoint(
        db,
        "INSERT OR IGNORE INTO symhow_cand SELECT e.source_node_id FROM edge e JOIN symhow_cand c ON c.id=e.target_node_id \
         WHERE e.type=1;",
    )?;

    // step 3.5: carry the candidate NODES across the re-index by name.
    db.exec("DROP TABLE IF EXISTS symhow_pending_cand;")?;
    db.exec("CREATE TABLE symhow_pending_cand(serialized_name TEXT PRIMARY KEY);")?;
    db.exec(
        "INSERT OR IGNORE INTO symhow_pending_cand(serialized_name) SELECT n.serialized_name FROM node n \
         JOIN symhow_cand c ON c.id=n.id;",
    )?;

    // step 4: definition-kind reset.
    db.exec("DROP TABLE IF EXISTS symhow_lost;")?;
    db.exec(
        "CREATE TEMP TABLE symhow_lost AS SELECT DISTINCT node_id FROM symhow_symbol_def WHERE file_node_id IN \
         (SELECT id FROM symhow_clear);",
    )?;
    db.exec("DELETE FROM symhow_symbol_def WHERE file_node_id IN (SELECT id FROM symhow_clear);")?;
    db.exec("DELETE FROM symbol WHERE id IN (SELECT node_id FROM symhow_lost) AND id NOT IN (SELECT node_id FROM symhow_symbol_def);")?;
    stats.symbols_reset = db.changes();

    // step 4b: reset node.type to UNKNOWN for every node in symhow_lost.
    db.exec(&format!(
        "UPDATE node SET type = {} WHERE id IN (SELECT node_id FROM symhow_lost);",
        node_kind::UNKNOWN
    ))?;
    db.exec("DELETE FROM symhow_tu WHERE file_node_id IN (SELECT id FROM symhow_clear) OR path IN (SELECT path FROM symhow_paths);")?;

    // step 5: cleared file nodes (cascades).
    db.exec("DELETE FROM element WHERE id IN (SELECT id FROM symhow_clear);")?;
    stats.files = db.changes();

    // step 6: candidate non-MEMBER edges with no remaining occurrence.
    db.exec(
        "DELETE FROM element WHERE id IN (SELECT e.id FROM edge e WHERE e.type<>1 AND e.id IN (SELECT id FROM symhow_cand) \
         AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.element_id=e.id));",
    )?;
    stats.edges = db.changes();

    db.exec("DELETE FROM source_location WHERE file_node_id NOT IN (SELECT id FROM node);")?;
    stats.locations = db.changes();

    // steps 7-9, restricted to symhow_cand.
    let sweep = dead_node_and_orphan_sweep(db, "JOIN symhow_cand c ON c.id=n.id", true)?;
    stats.edges += sweep.edges;
    stats.locations = sweep.locations;
    stats.nodes = sweep.nodes;
    stats.local_symbols = sweep.local_symbols;
    stats.errors = sweep.errors;
    stats.non_indexed_files = sweep.non_indexed_files;
    stats.orphan_elements = sweep.orphan_elements;

    db.exec("DROP TABLE IF EXISTS symhow_lost;")?;
    db.exec("DROP TABLE IF EXISTS symhow_cand;")?;
    db.exec("DROP TABLE IF EXISTS symhow_clear;")?;
    db.exec("DROP TABLE IF EXISTS symhow_paths;")?;

    db.commit()?;
    Ok(stats)
}

/// Performs the full clear + GC algorithm against `plan`, inside a single
/// BEGIN...COMMIT transaction (rolls back and rethrows on any error). `db`
/// must have been opened with `foreign_keys=true`.
pub fn invalidate(db: &mut WritableDb, plan: &ClearPlan) -> Result<GcStats> {
    db.begin()?;
    match invalidate_inner(db, plan) {
        Ok(stats) => Ok(stats),
        Err(e) => {
            let _ = db.rollback();
            Err(e)
        }
    }
}

/// Runs the same dead-node/local-symbol/error/orphan-element sweep
/// `invalidate()` does, but UNRESTRICTED to the recorded candidate set (by
/// serialized_name, carried across the re-index).
pub fn sweep_orphans(db: &mut WritableDb) -> Result<GcStats> {
    db.begin()?;
    let inner = |db: &WritableDb| -> Result<GcStats> {
        db.exec("CREATE TABLE IF NOT EXISTS symhow_pending_cand(serialized_name TEXT PRIMARY KEY);")?;
        let stats = dead_node_and_orphan_sweep(
            db,
            "JOIN symhow_pending_cand p ON p.serialized_name=n.serialized_name",
            false,
        )?;
        db.exec("DROP TABLE IF EXISTS symhow_pending_cand;")?;
        Ok(stats)
    };

    match inner(db) {
        Ok(stats) => {
            db.commit()?;
            Ok(stats)
        }
        Err(e) => {
            let _ = db.rollback();
            Err(Error::new(e.0))
        }
    }
}