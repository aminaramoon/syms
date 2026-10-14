//! Port of test/unit/test_notes_sync.cpp (fixture-dependent). Like the C++
//! file, this is ONE test (not several independent ones) -- every step after
//! the first depends on state the previous step left behind (the generated
//! notes dir, the synced DB). Steps that shell out to the `symhow` BINARY are
//! skipped with TODO(parity) comments; everything that exercises the library
//! APIs directly (syncNotes() vs an independent oracle, the --json envelope,
//! openNotes()'s overlay, determinism) is ported in full.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use symhow::db::Database;
use symhow::notes::{
    body_hash_for_node, current_commit, list_symbols, match_block, open_notes, parse_notes_file, sync_exit_code,
    sync_json, sync_notes, MatchKind, SyncResult,
};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../build/fixture.srctrldb".to_string())
}

fn read_file(path: &str) -> String {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .expect("read")
}

fn write_file(path: &str, content: &str) {
    std::fs::write(path, content).expect("write");
}

fn dump_note_table(db_path: &str) -> String {
    let db = Database::open(db_path, 0).unwrap();
    let mut out = String::new();
    db.run(
        "SELECT serialized_name, brief, what, stale FROM symhow_note ORDER BY serialized_name",
        [],
        |row| {
            let sn: String = row.get::<_, Option<String>>(0)?.unwrap_or_default();
            let brief: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let what: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let stale: i32 = row.get::<_, i32>(3)?;
            out.push_str(&format!("{}|{}|{}|{}\n", sn, brief, what, stale));
            Ok(())
        },
    )
    .unwrap();
    out
}

// Recomputes the exact SyncResult a correct sync_notes() should have
// produced, using the same public match_block()/body_hash_for_node() APIs,
// but entirely independently of sync.rs's own bookkeeping.
struct Oracle {
    blocks: usize,
    matched: usize,
    unmatched: usize,
    ambiguous: usize,
    duplicate_target: usize,
    filled: usize,
    stale: usize,
    unanchored: usize,
}

fn build_oracle(db: &Database, notes_dir: &str) -> Oracle {
    let mut o = Oracle {
        blocks: 0,
        matched: 0,
        unmatched: 0,
        ambiguous: 0,
        duplicate_target: 0,
        filled: 0,
        stale: 0,
        unanchored: 0,
    };
    let mut matched_nodes: HashSet<i64> = HashSet::new();

    let mut md_files: Vec<String> = Vec::new();
    collect_md_files(Path::new(notes_dir), &mut md_files);
    md_files.sort();

    for full in &md_files {
        let rel_path = path_relative_to(full, notes_dir);
        let parsed = parse_notes_file(&read_file(full), &rel_path, false);
        for b in &parsed.blocks {
            o.blocks += 1;
            let mo = match_block(db, b).unwrap();
            if mo.kind == MatchKind::Unmatched {
                o.unmatched += 1;
                continue;
            }
            if mo.kind == MatchKind::Ambiguous {
                o.ambiguous += 1;
                continue;
            }
            if !matched_nodes.insert(mo.node_id) {
                o.duplicate_target += 1;
                continue;
            }
            o.matched += 1;
            if b.filled() {
                o.filled += 1;
            }
            let current_hash = body_hash_for_node(db, mo.node_id).unwrap();
            if b.body_hash.is_empty() {
                o.unanchored += 1;
            } else if !current_hash.is_empty() && b.body_hash != current_hash {
                o.stale += 1;
            }
        }
    }
    o
}

fn collect_md_files(dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}

fn path_relative_to(full: &str, base: &str) -> String {
    Path::new(full)
        .strip_prefix(base)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| full.to_string())
}

/// `symhow notes symbols --json`'s output envelope (runNotesSymbols in
/// Cli.cpp), key-for-key, built from the library list_symbols() instead of
/// the (not-yet-ported) CLI verb.
// TODO(parity): needs CLI -- once the `symhow` binary is ported, step 1
// should shell out to `symhow notes symbols -d DB --json` exactly like the
// C++ test does.
fn notes_symbols_json(db_path: &str) -> String {
    let db = Database::open(db_path, 0).unwrap();
    let commit = current_commit(&db);
    let rows = list_symbols(&db, 0, &[]).unwrap();
    let symbols: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "source_path": r.source_path,
                "qualified_name": r.qualified_name,
                "signature": r.signature,
                "body_hash": r.body_hash,
                "commit": commit,
                "node_id": r.node_id,
            })
        })
        .collect();
    serde_json::json!({
        "symhow_version": "0.1.0",
        "schema": "symhow.notes-symbols/1",
        "database": db_path,
        "symbols": symbols,
    })
    .to_string()
}

#[test]
fn notes_sync_end_to_end_sync_oracle_json_overlay_determinism() {
    let tmp = std::env::temp_dir().join("symhow_test_notes_sync");
    std::fs::create_dir_all(&tmp).unwrap();

    let db_path = tmp.join("notes_sync.srctrldb");
    std::fs::copy(fixture_db_path(), &db_path).unwrap();

    let notes_dir = tmp.join("notes_dir");
    let notes_dir_str = notes_dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&notes_dir);

    // --- step 1: `symhow notes symbols --json` -> generator -------------
    let syms_json = notes_symbols_json(db_path.to_string_lossy().as_ref());
    let syms_json_parsed: serde_json::Value = serde_json::from_str(&syms_json).unwrap();
    assert_eq!(syms_json_parsed["schema"], "symhow.notes-symbols/1");
    assert!(!syms_json_parsed["symbols"].as_array().unwrap().is_empty());

    let syms_path = tmp.join("symbols.json");
    write_file(&syms_path.to_string_lossy(), &syms_json);

    let generator_path = "../scripts/symhow-notes-fake.py";
    let gen = Command::new("python3")
        .args([
            generator_path,
            "--symbols-json",
            syms_path.to_string_lossy().as_ref(),
            "--notes-dir",
            &notes_dir_str,
            "--seed",
            "11",
            "--fill-ratio",
            "0.7",
            "--stale-ratio",
            "0.2",
            "--drop-hash-ratio",
            "0.15",
            "--overloads",
        ])
        .output();
    let gen = match gen {
        Ok(g) => g,
        Err(_) => {
            // python3 not available -- skip like the C++ WARNs and returns.
            eprintln!("python3 not available -- skipping fake generator test");
            return;
        }
    };
    assert!(gen.status.success(), "generator failed: {}", String::from_utf8_lossy(&gen.stderr));
    assert!(notes_dir.is_dir());

    // --- step 2: sync_notes() (in-process) vs. an independent oracle ---
    let sync1: SyncResult = sync_notes(db_path.to_string_lossy().as_ref(), &notes_dir_str, false).unwrap();
    assert!(sync1.counts.files > 0);
    assert!(sync1.counts.blocks > 0);
    assert_eq!(
        sync1.counts.blocks,
        sync1.counts.matched + sync1.counts.unmatched + sync1.counts.ambiguous + sync1.counts.duplicate_target
    );

    let db_after_sync1 = Database::open(db_path.to_string_lossy().as_ref(), 0).unwrap();
    let oracle = build_oracle(&db_after_sync1, &notes_dir_str);
    assert_eq!(oracle.blocks, sync1.counts.blocks);
    assert_eq!(oracle.matched, sync1.counts.matched);
    assert_eq!(oracle.unmatched, sync1.counts.unmatched);
    assert_eq!(oracle.ambiguous, sync1.counts.ambiguous);
    assert_eq!(oracle.duplicate_target, sync1.counts.duplicate_target);
    assert_eq!(oracle.filled, sync1.counts.filled);
    assert_eq!(oracle.stale, sync1.counts.stale);
    assert_eq!(oracle.unanchored, sync1.counts.unanchored);

    // `--overloads` + a fixture with genuinely overloaded functions (e.g.
    // `f`, `helper`, `Accessor::value`) guarantees at least one ambiguous
    // heading; genuinely non-overloaded originals' injected twins guarantee
    // at least one duplicate-target.
    assert!(sync1.counts.ambiguous > 0);
    assert!(sync1.counts.duplicate_target > 0);
    assert!(sync1.counts.matched > 0);
    assert!(sync1.counts.filled > 0);
    assert!(sync1.counts.stale > 0);
    assert!(sync1.counts.unanchored > 0);

    // `symhow notes sync --json` (CLI) reports the exact same counts -- here
    // via the ported sync_json() envelope (exact key names from Cli.cpp's
    // runNotesSync) and sync_exit_code() contract.
    let cli_sync_json = sync_json(&sync1, "0.1.0", db_path.to_string_lossy().as_ref(), &notes_dir_str);
    assert_eq!(cli_sync_json["schema"], "symhow.notes-sync/1");
    assert_eq!(cli_sync_json["database"].as_str().unwrap(), db_path.to_string_lossy().as_ref());
    assert!(cli_sync_json["notes_dir"].as_str().unwrap().starts_with('/'));
    assert_eq!(cli_sync_json["counts"]["matched"], sync1.counts.matched);
    assert_eq!(cli_sync_json["counts"]["ambiguous"], sync1.counts.ambiguous);
    assert_eq!(cli_sync_json["counts"]["stale"], sync1.counts.stale);
    assert_eq!(cli_sync_json["counts"]["unanchored"], sync1.counts.unanchored);
    assert_eq!(cli_sync_json["counts"]["filled"], sync1.counts.filled);
    assert_eq!(cli_sync_json["counts"]["duplicate_target"], sync1.counts.duplicate_target);
    assert_eq!(cli_sync_json["counts"]["orphaned_files"], sync1.counts.orphaned_files);
    let problems = cli_sync_json["problems"].as_array().unwrap();
    assert_eq!(problems.len(), sync1.problems.len());
    for (pj, p) in problems.iter().zip(sync1.problems.iter()) {
        assert_eq!(pj["path"], p.path);
        assert_eq!(pj["line"], p.line);
        assert_eq!(pj["heading"], p.heading);
        assert_eq!(pj["kind"], p.kind);
        assert_eq!(pj["message"], p.message);
    }
    assert_eq!(sync_exit_code(false, &sync1), 0);
    assert_eq!(sync_exit_code(true, &sync1), 3); // --strict + problems -> 3

    // TODO(parity): needs CLI -- step 3's `symhow notes show` (text and
    // --json, `symhow.note/1` schema, --section what, exit 3 for an
    // unnoted symbol) and step 4's `symhow notes status --json`
    // (`symhow.notes-status/1`, counts.noted) are pure CLI verbs over the
    // same library calls (open_notes()); the library halves are covered
    // below (overlay step) and in notes_scaffold.rs.

    // --- step 5: overlay -- edit on disk WITHOUT re-syncing --------------
    // (the C++ test observes this through `notes show`; the same state is
    // visible through the library open_notes() overlay that verb calls.)
    let (filled_serialized_name, filled_what, notes_path) = {
        let db = Database::open(db_path.to_string_lossy().as_ref(), 0).unwrap();
        let mut filled_serialized_name = String::new();
        let mut filled_what = String::new();
        let mut notes_path = String::new();
        db.run(
            "SELECT serialized_name, what, notes_path FROM symhow_note WHERE filled = 1 LIMIT 1",
            [],
            |row| {
                filled_serialized_name = row.get::<_, Option<String>>(0)?.unwrap_or_default();
                filled_what = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                notes_path = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                Ok(())
            },
        )
        .unwrap();
        assert!(!filled_serialized_name.is_empty());
        assert!(!filled_what.is_empty(), "the filled pick must have a non-empty 'what' to edit");
        (filled_serialized_name, filled_what, notes_path)
    };

    let edited_file_full = notes_dir.join(&notes_path);
    let before_edit = read_file(&edited_file_full.to_string_lossy());
    assert!(before_edit.contains(&filled_what));
    let overlay_marker = "OVERLAY_MARKER_TEXT_symhow_notes_phase2_test";
    let after_edit = before_edit.replacen(&filled_what, overlay_marker, 1);
    write_file(&edited_file_full.to_string_lossy(), &after_edit);

    {
        let db = Database::open(db_path.to_string_lossy().as_ref(), 0).unwrap();
        let lookup = open_notes(&db).unwrap();
        assert_eq!(lookup.by_serialized_name[&filled_serialized_name].what, overlay_marker);
        assert!(!lookup.overlay_files.is_empty());
    }

    // Re-sync -> the hint goes away and the new text is persisted.
    sync_notes(db_path.to_string_lossy().as_ref(), &notes_dir_str, false).unwrap();
    {
        let db = Database::open(db_path.to_string_lossy().as_ref(), 0).unwrap();
        let lookup = open_notes(&db).unwrap();
        assert_eq!(lookup.by_serialized_name[&filled_serialized_name].what, overlay_marker);
        assert!(lookup.overlay_files.is_empty());
    }

    // --- step 6: determinism -- sync again, table dump identical --------
    let dump_after_resync = dump_note_table(db_path.to_string_lossy().as_ref());
    sync_notes(db_path.to_string_lossy().as_ref(), &notes_dir_str, false).unwrap();
    let dump_after_third_sync = dump_note_table(db_path.to_string_lossy().as_ref());
    assert_eq!(dump_after_resync, dump_after_third_sync);

    // --- step 7: disposable cache -- fresh index, same notes -> same rows
    // TODO(parity): needs CLI -- re-indexing the fixture
    // (`symhow index -c fixture_build/compile_commands.json -o fresh --force`)
    // goes through the srctrldb FFI write path, not the notes layer; covered
    // by test_notes_sync.cpp once the binary is ported.
}
