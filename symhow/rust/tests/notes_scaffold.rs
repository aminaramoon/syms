//! Port of test/unit/test_notes_scaffold.cpp (fixture-dependent). Like the
//! C++ file, one test whose steps build on the state the previous step left
//! behind: `symhow notes scaffold/accept/fmt` CLI-verb steps are skipped with
//! TODO(parity) comments (the binary isn't ported); everything that goes
//! through the library APIs (list_symbols/scaffold_notes/match_block/
//! body_hash_for_node/sync_notes) is ported in full, including the python
//! `--fill-existing` helper the C++ test also invokes directly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use symhow::db::Database;
use symhow::notes::{
    body_hash_for_node, list_symbols, match_block, parse_notes_file, placeholder_block_markdown, scaffold_notes,
    sync_notes, MatchKind, NoteBlock, ScaffoldOptions, TextSection,
};

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../cpp/build/fixture.srctrldb".to_string())
}

fn read_file(path: &str) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).expect("read")
}

fn count_md_files(dir: &str) -> usize {
    fn walk(dir: &Path, n: &mut usize) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, n);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                *n += 1;
            }
        }
    }
    let mut n = 0;
    walk(Path::new(dir), &mut n);
    n
}

// Snapshot of every *.md file's bytes under `dir`, keyed by path relative to
// `dir` -- used to assert "byte-for-byte unchanged" across a re-scaffold.
fn snapshot_files(dir: &str) -> BTreeMap<String, String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                let rel = path
                    .strip_prefix(base)
                    .map(|r| r.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.insert(rel, read_file(&path.to_string_lossy()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(Path::new(dir), Path::new(dir), &mut out);
    out
}

fn scaffold_all(db: &Database, notes_dir: &str) -> symhow::notes::ScaffoldResult {
    let all = list_symbols(db, 0, &[]).unwrap();
    scaffold_notes(db, &all, &ScaffoldOptions { notes_dir: notes_dir.to_string(), dry_run: false }).unwrap()
}

#[test]
fn notes_scaffold_end_to_end_scaffold_fill_sync_file_subset_corruption_mismatch() {
    let tmp = std::env::temp_dir().join("symhow_test_notes_scaffold");
    std::fs::create_dir_all(&tmp).unwrap();

    let db_path = tmp.join("scaffold.srctrldb");
    std::fs::copy(fixture_db_path(), &db_path).unwrap();
    let db_path_str = db_path.to_string_lossy().into_owned();

    let notes_dir = tmp.join("notes_dir");
    let notes_dir_str = notes_dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&notes_dir);

    let db = Database::open(&db_path_str, 0).unwrap();

    // --- step 1: `scaffold --all` -----------------------------------------
    // TODO(parity): needs CLI -- the C++ step shells out to
    // `symhow notes scaffold --all --json` (schema "symhow.notes-scaffold/1");
    // here the same code path runs in-process (the CLI's --all is exactly
    // list_symbols() + scaffold_notes()).
    let scaffold_result = scaffold_all(&db, &notes_dir_str);
    assert!(scaffold_result.total_appended > 0);
    assert!(notes_dir.is_dir());

    // One file per fixture source file with FUNCTION/METHOD definitions.
    let all_symbols = list_symbols(&db, 0, &[]).unwrap();
    let expected_files: BTreeSet<String> =
        all_symbols.iter().map(|r| format!("{}.md", r.source_path)).collect();
    assert_eq!(count_md_files(&notes_dir_str), expected_files.len());

    // Every scaffolded block has a `symhow-hash` matching body_hash_for_node()
    // and parses clean (no lint problems -- `symhow notes lint` is the same
    // parseNotesFile() call the CLI runs).
    fn walk_blocks(dir: &Path, base: &Path, db: &Database, out: &mut usize) {
        let entries = std::fs::read_dir(dir).unwrap();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_blocks(&path, base, db, out);
                continue;
            }
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let rel_path = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                let content = read_file(&path.to_string_lossy());
                let parsed = parse_notes_file(&content, &rel_path, false);
                assert!(parsed.problems.is_empty());
                for b in &parsed.blocks {
                    *out += 1;
                    let mo = match_block(db, b).unwrap();
                    assert_eq!(mo.kind, MatchKind::Matched, "block '{}' must match", b.heading);
                    assert_eq!(b.body_hash, body_hash_for_node(db, mo.node_id).unwrap());
                    assert!(!b.body_hash.is_empty());
                }
            }
        }
    }
    let mut total_blocks = 0usize;
    walk_blocks(&notes_dir, &notes_dir, &db, &mut total_blocks);
    assert_eq!(total_blocks, all_symbols.len());

    // --- step 2: second `scaffold --all` run -> byte-identical no-op ------
    let before = snapshot_files(&notes_dir_str);
    let scaffold_again = scaffold_all(&db, &notes_dir_str);
    assert_eq!(scaffold_again.total_appended, 0);
    let after = snapshot_files(&notes_dir_str);
    assert_eq!(before, after);

    // --- step 3: --fill-existing, then sync --------------------------------
    let fill = Command::new("python3")
        .args([
            "../cpp/scripts/symhow-notes-fake.py",
            "--fill-existing",
            &notes_dir_str,
            "--seed",
            "7",
            "--fill-ratio",
            "0.6",
        ])
        .output();
    let fill = match fill {
        Ok(f) => f,
        Err(_) => {
            eprintln!("python3 not available -- skipping the fill-existing part like the C++ test's WARN path");
            return;
        }
    };
    assert!(fill.status.success(), "fill-existing failed: {}", String::from_utf8_lossy(&fill.stderr));
    // "symhow-notes-fake: --fill-existing filled N blocks across M files" on stderr.
    let err = String::from_utf8_lossy(&fill.stderr);
    let marker = "filled ";
    let pos = err.find(marker).expect("fill-existing stderr marker");
    let rest = &err[pos + marker.len()..];
    let expected_filled: usize = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap();

    let sync_result = sync_notes(&db_path_str, &notes_dir_str, false).unwrap();
    assert_eq!(sync_result.counts.stale, 0);
    assert_eq!(sync_result.counts.unanchored, 0);
    assert_eq!(sync_result.counts.filled, expected_filled);

    // --- step 4: --file subset first, then --all -> only missing appended,
    // existing bytes preserved byte-for-byte -----------------------------
    let partial_notes_dir = tmp.join("notes_dir_partial");
    let partial_notes_dir_str = partial_notes_dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&partial_notes_dir);

    let chain_rows = list_symbols(&db, 0, &["**chain.cpp".to_string()]).unwrap();
    let partial_scaffold =
        scaffold_notes(&db, &chain_rows, &ScaffoldOptions { notes_dir: partial_notes_dir_str.clone(), dry_run: false })
            .unwrap();
    assert_eq!(partial_scaffold.files.len(), 1);
    assert_eq!(count_md_files(&partial_notes_dir_str), 1);
    let partial_before = snapshot_files(&partial_notes_dir_str);
    assert!(!partial_before.is_empty());

    let full_scaffold = scaffold_all(&db, &partial_notes_dir_str);
    assert!(full_scaffold.total_appended > 0);
    assert_eq!(count_md_files(&partial_notes_dir_str), expected_files.len());
    // The chain.cpp.md file scaffolded by --file is untouched (same symbols,
    // 0 appended for it specifically) -- its bytes must be byte-identical.
    let partial_after = snapshot_files(&partial_notes_dir_str);
    for (path, bytes) in &partial_before {
        assert_eq!(partial_after.get(path).expect("file still present"), bytes);
    }

    // --- step 5: a corrupted `symhow-hash` goes stale on sync -----------------
    // Find a note whose file we can edit (any matched, filled or not) and
    // corrupt ITS SPECIFIC symhow-hash comment (located via heading_line, not
    // just the first "symhow-hash: " in the file -- a file has many blocks)
    // directly on disk, then sync to observe it go stale.
    let (target_notes_path, target_qualified_name, target_heading_line) = {
        let db2 = Database::open(&db_path_str, 0).unwrap();
        let mut notes_path = String::new();
        let mut qualified_name = String::new();
        let mut heading_line = 0i32;
        db2.run(
            "SELECT notes_path, qualified_name, heading_line FROM symhow_note ORDER BY serialized_name LIMIT 1",
            [],
            |row| {
                notes_path = row.get::<_, Option<String>>(0)?.unwrap_or_default();
                qualified_name = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                heading_line = row.get::<_, i32>(2)?;
                Ok(())
            },
        )
        .unwrap();
        assert!(!notes_path.is_empty());
        assert!(heading_line > 0);
        (notes_path, qualified_name, heading_line)
    };
    // `target_qualified_name` is what the (TODO(parity)) `symhow notes
    // accept` step would take as its SYMBOL argument.
    let _target_qualified_name = target_qualified_name;

    let target_file_full = notes_dir.join(&target_notes_path);
    let target_content = read_file(&target_file_full.to_string_lossy());

    // Locate the byte offset of `target_heading_line`'s line (1-based), then
    // search for "symhow-hash: " starting there (it's on the very next line,
    // if present at all).
    let mut line_start = 0usize;
    for _ in 1..target_heading_line {
        line_start = target_content[line_start..]
            .find('\n')
            .map(|p| line_start + p + 1)
            .expect("heading line exists");
    }
    let hash_pos = target_content[line_start..]
        .find("symhow-hash: ")
        .map(|p| line_start + p)
        .expect("symhow-hash on/after the heading line");
    // Sanity: this occurrence must be before the next "## " block starts.
    if let Some(next_block_pos) = target_content[line_start..].find("\n## ").map(|p| line_start + p) {
        assert!(hash_pos < next_block_pos);
    }
    let hash_value_start = hash_pos + "symhow-hash: ".len();
    // Flip the hash's first hex digit to something that can't collide.
    let mut corrupted: Vec<u8> = target_content.bytes().collect();
    corrupted[hash_value_start] = if corrupted[hash_value_start] == b'0' { b'1' } else { b'0' };
    std::fs::write(&target_file_full, &corrupted).unwrap();

    let sync_after_corrupt = sync_notes(&db_path_str, &notes_dir_str, false).unwrap();
    assert!(sync_after_corrupt.counts.stale >= 1);

    // TODO(parity): needs CLI -- the C++ step then runs `symhow notes accept`
    // (which re-anchors the corrupted symhow-hash to the current body hash,
    // refusing non-canonical files) and checks `notes status --json` reports
    // 0 stale. Accept is the one notes verb that rewrites the markdown
    // comment in place; port it with the CLI. Here, restore the un-corrupted
    // bytes (what accept would have produced) so the notes dir is left in
    // the same stale-free state for the rest of the test.
    std::fs::write(&target_file_full, &target_content).unwrap();
    let sync_after_restore = sync_notes(&db_path_str, &notes_dir_str, false).unwrap();
    assert_eq!(sync_after_restore.counts.stale, 0);

    // --- step 6: `fmt --check` -----------------------------------------
    // TODO(parity): needs CLI -- `symhow notes fmt --check` (canonical
    // re-render, exit 3 when any file differs) and the accept-refuses-
    // non-canonical-files regression live in Cli.cpp. The underlying
    // fixed-point (renderFile(parseNotesFile(x)) == x for canonical bytes) is
    // covered by tests/notes_markdown.rs and every scaffolded file above.

    // --- step 7: signature-mismatch ----------------------------------------
    // level1 (chain.cpp) is a single, non-overloaded candidate -- write a
    // block for it with a deliberately wrong `### signature` and confirm
    // match_block() still matches it but flags signature_mismatch, and
    // sync_notes() surfaces it as a "signature-mismatch" problem.
    let mismatch_block = NoteBlock {
        heading: "level1".to_string(),
        signature: TextSection {
            present: true,
            text: "double level1(double, double)".to_string(), // wrong on purpose
            ..Default::default()
        },
        ..Default::default()
    };
    let mismatch_outcome = match_block(&db, &mismatch_block).unwrap();
    assert_eq!(mismatch_outcome.kind, MatchKind::Matched);
    assert!(mismatch_outcome.signature_mismatch);
    assert!(!mismatch_outcome.detail.is_empty());

    let mismatch_dir = tmp.join("notes_dir_mismatch");
    let mismatch_dir_str = mismatch_dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&mismatch_dir);
    std::fs::create_dir_all(&mismatch_dir).unwrap();
    std::fs::write(
        mismatch_dir.join("mismatch.cpp.md"),
        format!(
            "# mismatch.cpp\n\n{}\n",
            placeholder_block_markdown("level1", "double level1(double, double)", "", "")
        ),
    )
    .unwrap();

    let mismatch_sync = sync_notes(&db_path_str, &mismatch_dir_str, false).unwrap();
    assert!(mismatch_sync.counts.signature_mismatch >= 1);
    assert!(mismatch_sync.problems.iter().any(|p| p.kind == "signature-mismatch"));

    // Re-sync the real notes_dir so it's left in a clean, stale-free state
    // (mismatch_dir above pointed sync_notes()/meta.symhow_notes_dir at itself).
    let final_sync = sync_notes(&db_path_str, &notes_dir_str, false).unwrap();
    assert_eq!(final_sync.counts.stale, 0);
}
