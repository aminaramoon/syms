//! `symhow notes VERB ...` (Cli.cpp's runNotes* family): lint, sync, show,
//! status, symbols, scaffold, accept, fmt -- function-level knowledge
//! attached to the symbol graph (docs/NOTES_DESIGN.md).

use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::cli::query_cmd::print_cache_outdated_hint;
use crate::cli::{absolute_raw, collect_md_files, relative_to_dir, symhow_version};
use crate::common::glob;
use crate::common::Result;
use crate::db::enums::{node_kind, node_kind_from_string, node_kind_to_string};
use crate::db::meta::get_meta;
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;
use crate::notes::markdown::{parse_notes_file, render_block, render_file};
use crate::notes::note_model::{ListSection, NoteBlock, TextSection};
use crate::notes::scaffold::{
    list_symbols, row_for_node, scaffold_notes, ScaffoldOptions, SymbolRow,
};
use crate::notes::sync::{
    match_block, sync_exit_code, sync_json, sync_notes, MatchKind, MatchOutcome, SyncProblem,
};
use crate::notes::{body_hash_for_node, current_commit, open_notes};
use crate::out::json_writer::{JsonWriter, QueryContext};
use crate::out::text_writer::{write_candidate_table, CandidateRow};
use crate::query::{resolve_symbol, QueryEngine, ResolveOutcome};

fn read_file_bytes(path: &str) -> String {
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn is_directory(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

// `symhow notes show` and `scaffold` build CandidateRow lists from the same
// ResolvedSymbol set Cli.cpp's toCandidateRows() handles.
fn to_candidate_rows(
    engine: &QueryEngine,
    candidates: &[crate::query::ResolvedSymbol],
) -> Result<Vec<CandidateRow>> {
    let mut rows = Vec::with_capacity(candidates.len());
    for c in candidates {
        let mut row = CandidateRow {
            id: c.id,
            kind: node_kind_to_string(c.kind),
            name: decode_serialized_name(&c.serialized_name).display_name(),
            location: String::new(),
        };
        let def = engine.definition(&[c.id])?;
        if let Some(loc) = def.locations.first() {
            row.location = format!("{}:{}", loc.file_path, loc.start_line);
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Resolves the notes directory a DB-backed notes verb should use, absent an
/// explicit --notes-dir: `meta.symhow_notes_dir` (set by a previous
/// `symhow notes sync`), else `<meta.symhow_repo_root>/notes`. Returns "" if
/// neither is available (caller reports a usage error). Mirrors Cli.cpp's
/// resolveNotesDirDefault().
fn resolve_notes_dir_default(db_path: &str) -> String {
    if let Ok(db) = Database::open(db_path, 0) {
        let stored = get_meta(db.conn(), "symhow_notes_dir");
        if let Some(dir) = stored {
            if !dir.is_empty() {
                return dir;
            }
        }
        let repo_root = get_meta(db.conn(), "symhow_repo_root");
        if let Some(root) = repo_root {
            if !root.is_empty() {
                return format!("{}/notes", root.trim_end_matches('/'));
            }
        }
    }
    // Falls through to "" -- the caller's own Database open (right after)
    // reports the real error message.
    String::new()
}

/// Returns the raw (still on-disk) content of `notes_dir`/`rel_path`, or
/// None if it doesn't exist. Shared by `symhow notes fmt` and `symhow notes
/// accept`'s canonical-form guard.
fn read_notes_file(notes_dir: &str, rel_path: &str) -> Option<String> {
    let full = std::path::Path::new(notes_dir).join(rel_path);
    if !full.is_file() {
        return None;
    }
    Some(read_file_bytes(&full.to_string_lossy()))
}

fn write_notes_file(notes_dir: &str, rel_path: &str, content: &str) {
    let full = std::path::Path::new(notes_dir).join(rel_path);
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut out) = std::fs::File::create(&full) {
        let _ = out.write_all(content.as_bytes());
    }
}

// --- lint ---------------------------------------------------------------------

/// `symhow notes lint --notes-dir DIR [--strict] [--json]` -- walks DIR
/// recursively for `*.md`, parses each with parse_notes_file (no DB
/// involved), and reports every Problem. Exit codes: 0 no problems, 1 usage
/// error, 3 problems found. Mirrors Cli.cpp's runNotesLint().
fn run_notes_lint(args: &[String], start_idx: usize) -> i32 {
    let mut notes_dir = String::new();
    let mut strict = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--notes-dir" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes lint: --notes-dir requires a value");
                    return 1;
                }
                notes_dir = args[i + 1].clone();
                i += 1;
            }
            "--strict" => {
                strict = true;
            }
            "--json" => {
                want_json = true;
            }
            "-h" | "--help" => {
                println!("Usage: symhow notes lint --notes-dir DIR [--strict] [--json]");
                return 0;
            }
            _ => {
                eprintln!("symhow notes lint: unknown option '{}'", arg);
                return 1;
            }
        }
        i += 1;
    }

    if notes_dir.is_empty() {
        eprintln!("symhow notes lint: missing required option --notes-dir DIR");
        return 1;
    }
    if !is_directory(&notes_dir) {
        eprintln!("symhow notes lint: '{}' is not a directory", notes_dir);
        return 1;
    }

    struct Finding {
        path: String,
        line: i32,
        kind: String,
        message: String,
    }

    let mut md_files = collect_md_files(&notes_dir);
    md_files.sort();

    let mut total_blocks: usize = 0;
    let mut total_filled: usize = 0;
    let mut findings: Vec<Finding> = Vec::new();

    for full_path in &md_files {
        let content = read_file_bytes(full_path);
        let rel_path = relative_to_dir(full_path, &notes_dir);
        let parsed = parse_notes_file(&content, &rel_path, strict);

        total_blocks += parsed.blocks.len();
        for b in &parsed.blocks {
            if b.filled() {
                total_filled += 1;
            }
        }
        for p in &parsed.problems {
            findings.push(Finding {
                path: rel_path.clone(),
                line: p.line,
                kind: p.kind.clone(),
                message: p.message.clone(),
            });
        }
    }

    findings.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));

    if want_json {
        let problems: Vec<serde_json::Value> = findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "path": f.path,
                    "line": f.line,
                    "kind": f.kind,
                    "message": f.message,
                })
            })
            .collect();
        let j = serde_json::json!({
            "symhow_version": symhow_version(),
            "schema": "symhow.notes-lint/1",
            "notes_dir": notes_dir,
            "files": md_files.len(),
            "blocks": total_blocks,
            "filled": total_filled,
            "problems": problems,
        });
        println!("{}", j.to_string());
    } else {
        for f in &findings {
            println!("{}:{}: {}: {}", f.path, f.line, f.kind, f.message);
        }
        println!(
            "{} files, {} blocks ({} filled), {} problems",
            md_files.len(),
            total_blocks,
            total_filled,
            findings.len()
        );
    }

    if findings.is_empty() {
        0
    } else {
        3
    }
}

// --- sync ---------------------------------------------------------------------

/// `symhow notes sync -d DB [--notes-dir DIR] [--strict] [--json]` --
/// rebuilds the `symhow_note*` cache from the markdown tree. Mirrors
/// Cli.cpp's runNotesSync(); the --json envelope/exit-code contract lives in
/// symhow::notes::sync::{sync_json,sync_exit_code}.
fn run_notes_sync(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut notes_dir_flag = String::new();
    let mut strict = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes sync: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--notes-dir" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes sync: --notes-dir requires a value");
                    return 1;
                }
                notes_dir_flag = args[i + 1].clone();
                i += 1;
            }
            "--strict" => {
                strict = true;
            }
            "--json" => {
                want_json = true;
            }
            "-h" | "--help" => {
                println!("Usage: symhow notes sync -d DB [--notes-dir DIR] [--strict] [--json]");
                return 0;
            }
            _ => {
                eprintln!("symhow notes sync: unknown option '{}'", arg);
                return 1;
            }
        }
        i += 1;
    }

    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        }
    }
    if db_path.is_empty() {
        eprintln!("symhow notes sync: missing required option -d/--db PATH");
        return 1;
    }

    let notes_dir = if !notes_dir_flag.is_empty() {
        notes_dir_flag
    } else {
        resolve_notes_dir_default(&db_path)
    };
    if notes_dir.is_empty() {
        eprintln!(
            "symhow notes sync: no notes dir given and none stored (nor a repo root) in '{}' -- pass --notes-dir DIR",
            db_path
        );
        return 1;
    }

    let result = match sync_notes(&db_path, &notes_dir, strict) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("symhow notes sync: {}", e.0);
            return 4;
        }
    };

    if want_json {
        println!(
            "{}",
            sync_json(&result, symhow_version(), &db_path, &notes_dir).to_string()
        );
    } else {
        for p in &result.problems {
            println!("{}:{}: {}: {}", p.path, p.line, p.kind, p.message);
        }
        println!(
            "{} files, {} blocks: {} matched ({} filled, {} stale, {} unanchored, {} signature-mismatch), {} unmatched, {} ambiguous, {} duplicate-target, {} orphaned-file, {} lint problems",
            result.counts.files,
            result.counts.blocks,
            result.counts.matched,
            result.counts.filled,
            result.counts.stale,
            result.counts.unanchored,
            result.counts.signature_mismatch,
            result.counts.unmatched,
            result.counts.ambiguous,
            result.counts.duplicate_target,
            result.counts.orphaned_files,
            result.counts.lint_problems
        );
    }

    sync_exit_code(strict, &result)
}

// --- show ---------------------------------------------------------------------

/// `symhow notes show -d DB SYMBOL [--section SEC] [--json]`. Resolves SYMBOL
/// exactly like `symhow query`, then looks the note up via the overlay-aware
/// open_notes(). Exit codes: 0 ok, 1 usage, 2 ambiguous, 3 not found / no
/// note, 4 db error. Mirrors Cli.cpp's runNotesShow().
fn run_notes_show(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut section = String::new();
    let mut want_json = false;
    let mut spec = String::new();
    let mut have_spec = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes show: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--section" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes show: --section requires a value");
                    return 1;
                }
                section = args[i + 1].clone();
                i += 1;
            }
            "--json" => {
                want_json = true;
            }
            "-h" | "--help" => {
                println!(
                    "Usage: symhow notes show -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]"
                );
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                eprintln!("symhow notes show: unknown option '{}'", arg);
                return 1;
            }
            _ => {
                if have_spec {
                    eprintln!("symhow notes show: unexpected extra argument '{}'", arg);
                    return 1;
                }
                spec = arg.to_string();
                have_spec = true;
            }
        }
        i += 1;
    }

    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        } else {
            db_path = "./symhow.srctrldb".to_string();
        }
    }
    if !have_spec {
        eprintln!("symhow notes show: missing required SYMBOL argument");
        return 1;
    }
    const K_SECTIONS: [&str; 8] = [
        "brief", "what", "how", "signature", "pre", "post", "bugs", "suggestions",
    ];
    if !section.is_empty() && !K_SECTIONS.contains(&section.as_str()) {
        eprintln!("symhow notes show: unknown --section '{}'", section);
        return 1;
    }

    let db = match Database::open(db_path.as_str(), 0) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow notes show: {}", e.0);
            return 4;
        }
    };

    let body = || -> Result<i32> {
        let engine = QueryEngine::new(&db)?;

        let json_writer = if want_json {
            Some(JsonWriter::new(&db, &engine, symhow_version()))
        } else {
            None
        };

        let mut json_ctx = QueryContext {
            verb: "show".to_string(),
            db: db_path.clone(),
            symbol_or_text_or_path: spec.clone(),
            ..Default::default()
        };

        let rr = resolve_symbol(&db, &spec, None, None)?;
        if rr.outcome == ResolveOutcome::Ambiguous {
            if let Some(writer) = &json_writer {
                println!("{}", writer.ambiguous(&json_ctx, &rr.candidates)?.to_string());
            } else {
                println!(
                    "symhow notes show: '{}' is ambiguous ({} candidates):",
                    spec,
                    rr.candidates.len()
                );
                print!("{}", write_candidate_table(&to_candidate_rows(&engine, &rr.candidates)?));
            }
            return Ok(2);
        }
        if rr.outcome == ResolveOutcome::NotFound {
            let hits = engine.search(&spec, None, 5)?;
            if let Some(writer) = &json_writer {
                println!("{}", writer.not_found(&json_ctx, &hits)?.to_string());
            } else {
                eprintln!("symhow notes show: symbol '{}' not found.", spec);
            }
            return Ok(3);
        }

        let sym = &rr.candidates[0];
        json_ctx.symbol_ids = vec![sym.id];

        let lookup = open_notes(&db)?;
        print_cache_outdated_hint(&lookup.overlay_files);

        let note = lookup.by_serialized_name.get(&sym.serialized_name);
        let has_note = note.is_some();

        if want_json {
            let writer = json_writer.as_ref().unwrap();
            let def_result = engine.definition(&[sym.id])?;
            let def_json = writer.definition(&json_ctx, &def_result, 0)?;

            let note_json = note.map(|n| {
                serde_json::json!({
                    "brief": n.brief,
                    "what": n.what,
                    "how": n.how,
                    "signature": n.signature_text,
                    "pre": n.pre,
                    "post": n.post,
                    "bugs": n.bugs,
                    "suggestions": n.suggestions,
                    "path": n.notes_path,
                    "line": n.heading_line,
                    "filled": n.filled,
                    "stale": n.stale,
                    "note_hash": n.note_hash,
                    "current_body_hash": n.current_body_hash,
                    "commit": n.note_commit,
                })
            });

            let j = serde_json::json!({
                "symhow_version": symhow_version(),
                "schema": "symhow.note/1",
                "query": {
                    "verb": "show",
                    "db": db_path,
                    "symbol": spec,
                    "symbol_ids": json_ctx.symbol_ids,
                },
                "symbol": def_json.get("symbol").cloned().unwrap_or(serde_json::Value::Null),
                "note": note_json,
                "cache_outdated_files": lookup.overlay_files,
            });
            println!("{}", j.to_string());
            return Ok(if has_note { 0 } else { 3 });
        }

        let Some(n) = note else {
            eprintln!("symhow notes show: symbol '{}' has no note.", spec);
            return Ok(3);
        };

        if !section.is_empty() {
            match section.as_str() {
                "brief" => println!("{}", n.brief),
                "what" => println!("{}", n.what),
                "how" => println!("{}", n.how),
                "signature" => println!("{}", n.signature_text),
                _ => {
                    let items: &Vec<String> = if section == "pre" {
                        &n.pre
                    } else if section == "post" {
                        &n.post
                    } else if section == "bugs" {
                        &n.bugs
                    } else {
                        &n.suggestions
                    };
                    for (idx, item) in items.iter().enumerate() {
                        println!("{}. {}", idx + 1, item);
                    }
                }
            }
            return Ok(0);
        }

        let block = note_to_block(n);
        print!("{}\n", render_block(&block));
        Ok(0)
    };

    match body() {
        Ok(code) => code,
        // Cli.cpp's run() has no catch-all around the notes verbs (an
        // exception there would std::terminate); reporting + exit 1 is the
        // closest non-crashing equivalent for a Rust port.
        Err(e) => {
            eprintln!("symhow notes show: {}", e.0);
            1
        }
    }
}

/// Rebuilds a full NoteBlock from a StoredNote (used by `show`'s text mode
/// and `query context --notes-full`). Mirrors Cli.cpp's inline block
/// reconstruction.
fn note_to_block(n: &crate::notes::StoredNote) -> NoteBlock {
    NoteBlock {
        heading: n.qualified_name.clone(),
        body_hash: n.note_hash.clone(),
        commit: n.note_commit.clone(),
        brief: n.brief.clone(),
        what: TextSection {
            present: true,
            line: 0,
            text: n.what.clone(),
        },
        how: TextSection {
            present: true,
            line: 0,
            text: n.how.clone(),
        },
        signature: TextSection {
            present: true,
            line: 0,
            text: n.signature_text.clone(),
        },
        pre: ListSection {
            present: true,
            line: 0,
            items: n.pre.clone(),
        },
        post: ListSection {
            present: true,
            line: 0,
            items: n.post.clone(),
        },
        bugs: ListSection {
            present: true,
            line: 0,
            items: n.bugs.clone(),
        },
        suggestions: ListSection {
            present: true,
            line: 0,
            items: n.suggestions.clone(),
        },
        ..Default::default()
    }
}

// --- status -------------------------------------------------------------------

/// `symhow notes status -d DB [...]` -- coverage report over FUNCTION+METHOD
/// nodes. Mirrors Cli.cpp's runNotesStatus().
fn run_notes_status(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut want_stale = false;
    let mut want_unfilled = false;
    let mut want_unanchored = false;
    let mut want_problems = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes status: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--stale" => want_stale = true,
            "--unfilled" => want_unfilled = true,
            "--unanchored" => want_unanchored = true,
            "--problems" => want_problems = true,
            "--json" => want_json = true,
            "-h" | "--help" => {
                println!(
                    "Usage: symhow notes status -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]"
                );
                return 0;
            }
            _ => {
                eprintln!("symhow notes status: unknown option '{}'", arg);
                return 1;
            }
        }
        i += 1;
    }
    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        } else {
            db_path = "./symhow.srctrldb".to_string();
        }
    }

    let db = match Database::open(db_path.as_str(), 0) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow notes status: {}", e.0);
            return 4;
        }
    };

    let body = || -> Result<i32> {
        // "coverage universe": every FUNCTION|METHOD node with a `symbol`
        // row.
        let mut coverage_by_serialized_name: HashMap<String, i64> = HashMap::new();
        db.run(
            "SELECT n.id, n.serialized_name FROM node n JOIN symbol s ON s.id = n.id WHERE (n.type & ?) != 0",
            [node_kind::FUNCTION | node_kind::METHOD],
            |row| {
                let id: i64 = row.get(0)?;
                let name: Option<String> = row.get(1)?;
                coverage_by_serialized_name
                    .insert(name.unwrap_or_default(), id);
                Ok(())
            },
        )?;

        let lookup = open_notes(&db)?;
        print_cache_outdated_hint(&lookup.overlay_files);

        let mut noted: usize = 0;
        let mut filled: usize = 0;
        let mut stale: usize = 0;
        let mut unanchored: usize = 0;
        let mut signature_mismatch: usize = 0;
        let mut stale_list: Vec<String> = Vec::new();
        let mut unfilled_list: Vec<String> = Vec::new();
        let mut unanchored_list: Vec<String> = Vec::new();
        for (serialized_name, note) in &lookup.by_serialized_name {
            if !coverage_by_serialized_name.contains_key(serialized_name) {
                continue; // not a FUNCTION/METHOD -- outside the coverage universe
            }
            noted += 1;
            let entry = format!(
                "{} ({}:{})",
                note.qualified_name, note.notes_path, note.heading_line
            );
            if note.filled {
                filled += 1;
            } else {
                unfilled_list.push(entry.clone());
            }
            if note.stale {
                stale += 1;
                stale_list.push(entry.clone());
            }
            if note.note_hash.is_empty() {
                unanchored += 1;
                unanchored_list.push(entry);
            }
        }
        stale_list.sort();
        unfilled_list.sort();
        unanchored_list.sort();

        let mut problems_list: Vec<SyncProblem> = Vec::new();
        if get_meta(db.conn(), "symhow_notes_dir").is_some() {
            // symhow_note_problem doesn't exist yet (sync never ran) -- leave empty.
            let _ = db.run(
                "SELECT notes_path, line, heading, kind, message FROM symhow_note_problem ORDER BY notes_path, line",
                [],
                |row| {
                    problems_list.push(SyncProblem {
                        path: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        line: row.get::<_, i32>(1)?,
                        heading: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        kind: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        message: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    });
                    Ok(())
                },
            );
        }
        for p in &problems_list {
            if p.kind == "signature-mismatch" {
                signature_mismatch += 1;
            }
        }

        if want_json {
            let mut j = serde_json::json!({
                "symhow_version": symhow_version(),
                "schema": "symhow.notes-status/1",
                "database": db_path,
                "counts": {
                    "total": coverage_by_serialized_name.len(),
                    "noted": noted,
                    "filled": filled,
                    "stale": stale,
                    "unanchored": unanchored,
                    "signature_mismatch": signature_mismatch,
                    "problems": problems_list.len(),
                },
            });
            if want_stale {
                j["stale"] = serde_json::json!(stale_list);
            }
            if want_unfilled {
                j["unfilled"] = serde_json::json!(unfilled_list);
            }
            if want_unanchored {
                j["unanchored"] = serde_json::json!(unanchored_list);
            }
            if want_problems {
                let pj: Vec<serde_json::Value> = problems_list
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "path": p.path,
                            "line": p.line,
                            "heading": p.heading,
                            "kind": p.kind,
                            "message": p.message,
                        })
                    })
                    .collect();
                j["problems"] = serde_json::json!(pj);
            }
            j["cache_outdated_files"] = serde_json::json!(lookup.overlay_files);
            println!("{}", j.to_string());
            return Ok(0);
        }

        println!(
            "{} indexed FUNCTION/METHOD symbols, {} noted ({} filled, {} stale, {} unanchored, {} signature-mismatch), {} problems",
            coverage_by_serialized_name.len(),
            noted,
            filled,
            stale,
            unanchored,
            signature_mismatch,
            problems_list.len()
        );
        if want_stale {
            println!("-- stale --");
            for s in &stale_list {
                println!("{}", s);
            }
        }
        if want_unfilled {
            println!("-- unfilled --");
            for s in &unfilled_list {
                println!("{}", s);
            }
        }
        if want_unanchored {
            println!("-- unanchored --");
            for s in &unanchored_list {
                println!("{}", s);
            }
        }
        if want_problems {
            println!("-- problems --");
            for p in &problems_list {
                println!("{}:{}: {}: {}", p.path, p.line, p.kind, p.message);
            }
        }
        Ok(0)
    };

    match body() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow notes status: {}", e.0);
            1
        }
    }
}

// --- symbols ------------------------------------------------------------------

/// `symhow notes symbols -d DB [--kind K]... [--file GLOB]... [--json]` --
/// lists indexed symbols. Mirrors Cli.cpp's runNotesSymbols().
fn run_notes_symbols(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut kind_names: Vec<String> = Vec::new();
    let mut file_globs: Vec<String> = Vec::new();
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes symbols: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--kind" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes symbols: --kind requires a value");
                    return 1;
                }
                kind_names.push(args[i + 1].clone());
                i += 1;
            }
            "--file" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes symbols: --file requires a value");
                    return 1;
                }
                file_globs.push(args[i + 1].clone());
                i += 1;
            }
            "--json" => want_json = true,
            "-h" | "--help" => {
                println!(
                    "Usage: symhow notes symbols -d DB [--kind FUNCTION|METHOD|CLASS|STRUCT]... [--file GLOB] [--json]"
                );
                return 0;
            }
            _ => {
                eprintln!("symhow notes symbols: unknown option '{}'", arg);
                return 1;
            }
        }
        i += 1;
    }
    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        } else {
            db_path = "./symhow.srctrldb".to_string();
        }
    }

    let mut mask: i32 = 0;
    for name in &kind_names {
        match node_kind_from_string(name) {
            Some(k) => mask |= k,
            None => {
                eprintln!("symhow notes symbols: unknown --kind value '{}'", name);
                return 1;
            }
        }
    }
    if mask == 0 {
        mask = node_kind::FUNCTION | node_kind::METHOD;
    }

    let db = match Database::open(db_path.as_str(), 0) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow notes symbols: {}", e.0);
            return 4;
        }
    };

    let commit = current_commit(&db);
    let rows = match list_symbols(&db, mask, &file_globs) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("symhow notes symbols: {}", e.0);
            return 1;
        }
    };

    if want_json {
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
        let j = serde_json::json!({
            "symhow_version": symhow_version(),
            "schema": "symhow.notes-symbols/1",
            "database": db_path,
            "symbols": symbols,
        });
        println!("{}", j.to_string());
    } else {
        for r in &rows {
            println!("{}\t{}\t{}\t#{}", r.source_path, r.qualified_name, r.signature, r.node_id);
        }
        println!("{} symbols", rows.len());
    }
    0
}

// --- scaffold -----------------------------------------------------------------

/// `symhow notes scaffold -d DB [...] (--all | --file GLOB | --class QNAME |
/// SYMBOL) [...]` -- Phase 3. Mirrors Cli.cpp's runNotesScaffold().
fn run_notes_scaffold(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut notes_dir_flag = String::new();
    let mut all = false;
    let mut file_glob = String::new();
    let mut class_qname = String::new();
    let mut symbol_spec = String::new();
    let mut have_symbol = false;
    let mut kind_names: Vec<String> = Vec::new();
    let mut dry_run = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes scaffold: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--notes-dir" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes scaffold: --notes-dir requires a value");
                    return 1;
                }
                notes_dir_flag = args[i + 1].clone();
                i += 1;
            }
            "--all" => all = true,
            "--file" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes scaffold: --file requires a value");
                    return 1;
                }
                file_glob = args[i + 1].clone();
                i += 1;
            }
            "--class" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes scaffold: --class requires a value");
                    return 1;
                }
                class_qname = args[i + 1].clone();
                i += 1;
            }
            "--kind" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes scaffold: --kind requires a value");
                    return 1;
                }
                kind_names.push(args[i + 1].clone());
                i += 1;
            }
            "--dry-run" => dry_run = true,
            "--json" => want_json = true,
            "-h" | "--help" => {
                println!(
                    "Usage: symhow notes scaffold -d DB [--notes-dir DIR] (--all | --file SRC-GLOB | --class QNAME | SYMBOL)\n{}[--kind FUNCTION|METHOD]... [--dry-run] [--json]",
                    " ".repeat(26)
                );
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                eprintln!("symhow notes scaffold: unknown option '{}'", arg);
                return 1;
            }
            _ => {
                if have_symbol {
                    eprintln!("symhow notes scaffold: unexpected extra argument '{}'", arg);
                    return 1;
                }
                symbol_spec = arg.to_string();
                have_symbol = true;
            }
        }
        i += 1;
    }

    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        } else {
            db_path = "./symhow.srctrldb".to_string();
        }
    }

    let selector_count = (all as i32)
        + (!file_glob.is_empty() as i32)
        + (!class_qname.is_empty() as i32)
        + (have_symbol as i32);
    if selector_count != 1 {
        eprintln!(
            "symhow notes scaffold: exactly one of --all / --file GLOB / --class QNAME / SYMBOL is required"
        );
        return 1;
    }

    let mut mask: i32 = 0;
    for name in &kind_names {
        match node_kind_from_string(name) {
            Some(k) => mask |= k,
            None => {
                eprintln!("symhow notes scaffold: unknown --kind value '{}'", name);
                return 1;
            }
        }
    }

    let db = match Database::open(db_path.as_str(), 0) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow notes scaffold: {}", e.0);
            return 4;
        }
    };

    let body = || -> Result<i32> {
        let engine = QueryEngine::new(&db)?;

        let repo_root = get_meta(db.conn(), "symhow_repo_root").unwrap_or_default();
        let commit = current_commit(&db);

        let mut rows: Vec<SymbolRow> = Vec::new();

        if all {
            rows = list_symbols(&db, mask, &[])?;
        } else if !file_glob.is_empty() {
            rows = list_symbols(&db, mask, &[file_glob.clone()])?;
        } else if !class_qname.is_empty() {
            let class_kinds = [node_kind::CLASS, node_kind::STRUCT];
            let rr = resolve_symbol(&db, &class_qname, Some(&class_kinds), None)?;
            if rr.outcome == ResolveOutcome::Ambiguous {
                println!(
                    "symhow notes scaffold: --class '{}' is ambiguous ({} candidates):",
                    class_qname,
                    rr.candidates.len()
                );
                print!("{}", write_candidate_table(&to_candidate_rows(&engine, &rr.candidates)?));
                return Ok(2);
            }
            if rr.outcome == ResolveOutcome::NotFound {
                eprintln!("symhow notes scaffold: --class '{}' not found.", class_qname);
                return Ok(3);
            }
            let member_mask = if mask != 0 {
                mask
            } else {
                node_kind::FUNCTION | node_kind::METHOD
            };
            let members = engine.members(&[rr.candidates[0].id], member_mask)?;
            for n in &members.nodes {
                let row = row_for_node(&db, &engine, n.id, &repo_root, &commit)?;
                if row.node_id != 0 {
                    rows.push(row);
                }
            }
        } else {
            let kind_filter: Vec<i32> = kind_names
                .iter()
                .map(|name| node_kind_from_string(name).unwrap_or(0))
                .collect();
            let rr = resolve_symbol(
                &db,
                &symbol_spec,
                if kind_filter.is_empty() { None } else { Some(&kind_filter) },
                None,
            )?;
            if rr.outcome == ResolveOutcome::Ambiguous {
                println!(
                    "symhow notes scaffold: '{}' is ambiguous ({} candidates):",
                    symbol_spec,
                    rr.candidates.len()
                );
                print!("{}", write_candidate_table(&to_candidate_rows(&engine, &rr.candidates)?));
                return Ok(2);
            }
            if rr.outcome == ResolveOutcome::NotFound {
                eprintln!("symhow notes scaffold: symbol '{}' not found.", symbol_spec);
                return Ok(3);
            }
            let row = row_for_node(&db, &engine, rr.candidates[0].id, &repo_root, &commit)?;
            if row.node_id == 0 {
                eprintln!(
                    "symhow notes scaffold: symbol '{}' has no definition location.",
                    symbol_spec
                );
                return Ok(3);
            }
            rows.push(row);
        }

        let notes_dir = if !notes_dir_flag.is_empty() {
            notes_dir_flag.clone()
        } else {
            resolve_notes_dir_default(&db_path)
        };
        if notes_dir.is_empty() {
            eprintln!(
                "symhow notes scaffold: no notes dir given and none stored (nor a repo root) in '{}' -- pass --notes-dir DIR",
                db_path
            );
            return Ok(1);
        }

        let opts = ScaffoldOptions {
            notes_dir: notes_dir.clone(),
            dry_run,
        };
        let result = scaffold_notes(&db, &rows, &opts)?;

        if want_json {
            let files: Vec<serde_json::Value> = result
                .files
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "path": f.path,
                        "file_created": f.file_created,
                        "appended": f.appended,
                        "skipped_existing": f.skipped_existing,
                    })
                })
                .collect();
            let j = serde_json::json!({
                "symhow_version": symhow_version(),
                "schema": "symhow.notes-scaffold/1",
                "database": db_path,
                "notes_dir": absolute_raw(&notes_dir),
                "dry_run": dry_run,
                "symbols_selected": rows.len(),
                "files": files,
                "totals": {
                    "files_created": result.files_created,
                    "files_touched": result.files_touched,
                    "appended": result.total_appended,
                    "skipped_existing": result.total_skipped,
                },
            });
            println!("{}", j.to_string());
        } else {
            for f in &result.files {
                println!(
                    "{}: {}{} appended, {} skipped-existing",
                    f.path,
                    if f.file_created { "(new) " } else { "" },
                    f.appended,
                    f.skipped_existing
                );
            }
            println!(
                "{} symbols selected: {} files created, {} files touched, {} blocks appended, {} skipped-existing",
                rows.len(),
                result.files_created,
                result.files_touched,
                result.total_appended,
                result.total_skipped
            );
        }
        Ok(0)
    };

    match body() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow notes scaffold: {}", e.0);
            1
        }
    }
}

// --- fmt ----------------------------------------------------------------------

/// `symhow notes fmt --notes-dir DIR [--check] [--json]` -- canonical
/// re-render of every `*.md` under DIR; `--check` reports what would change
/// without writing, exit 3 if anything would. No DB involved. Mirrors
/// Cli.cpp's runNotesFmt().
fn run_notes_fmt(args: &[String], start_idx: usize) -> i32 {
    let mut notes_dir = String::new();
    let mut check_only = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--notes-dir" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes fmt: --notes-dir requires a value");
                    return 1;
                }
                notes_dir = args[i + 1].clone();
                i += 1;
            }
            "--check" => check_only = true,
            "--json" => want_json = true,
            "-h" | "--help" => {
                println!("Usage: symhow notes fmt --notes-dir DIR [--check] [--json]");
                return 0;
            }
            _ => {
                eprintln!("symhow notes fmt: unknown option '{}'", arg);
                return 1;
            }
        }
        i += 1;
    }
    if notes_dir.is_empty() {
        eprintln!("symhow notes fmt: missing required option --notes-dir DIR");
        return 1;
    }
    if !is_directory(&notes_dir) {
        eprintln!("symhow notes fmt: '{}' is not a directory", notes_dir);
        return 1;
    }

    let mut md_files = collect_md_files(&notes_dir);
    md_files.sort();

    let mut changed: Vec<String> = Vec::new();
    for full in &md_files {
        let rel_path = relative_to_dir(full, &notes_dir);
        let content = read_file_bytes(full);
        let parsed = parse_notes_file(&content, &rel_path, false);
        let rendered = render_file(&parsed);
        if rendered != content {
            if !check_only {
                write_notes_file(&notes_dir, &rel_path, &rendered);
            }
            changed.push(rel_path);
        }
    }

    if want_json {
        let j = serde_json::json!({
            "symhow_version": symhow_version(),
            "schema": "symhow.notes-fmt/1",
            "notes_dir": notes_dir,
            "check": check_only,
            "files": md_files.len(),
            "changed": changed,
        });
        println!("{}", j.to_string());
    } else {
        for p in &changed {
            println!("{}{}", if check_only { "would reformat: " } else { "reformatted: " }, p);
        }
        println!(
            "{} files, {} {}",
            md_files.len(),
            changed.len(),
            if check_only { "would change" } else { "reformatted" }
        );
    }

    if check_only && !changed.is_empty() {
        3
    } else {
        0
    }
}

// --- accept -------------------------------------------------------------------

/// `symhow notes accept -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]` --
/// rewrites ONLY the `symhow-hash`/`symhow-commit` metadata comment of the
/// matching stale block(s), refusing (per file) any notes file that isn't
/// already in its render_file() fixed point. Re-runs sync_notes() afterward.
/// Mirrors Cli.cpp's runNotesAccept().
fn run_notes_accept(args: &[String], start_idx: usize) -> i32 {
    let mut db_path = String::new();
    let mut notes_dir_flag = String::new();
    let mut all = false;
    let mut file_glob = String::new();
    let mut symbol_spec = String::new();
    let mut have_symbol = false;
    let mut want_json = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes accept: '{}' requires a value", arg);
                    return 1;
                }
                db_path = args[i + 1].clone();
                i += 1;
            }
            "--notes-dir" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes accept: --notes-dir requires a value");
                    return 1;
                }
                notes_dir_flag = args[i + 1].clone();
                i += 1;
            }
            "--all" => all = true,
            "--file" => {
                if i + 1 >= args.len() {
                    eprintln!("symhow notes accept: --file requires a value");
                    return 1;
                }
                file_glob = args[i + 1].clone();
                i += 1;
            }
            "--json" => want_json = true,
            "-h" | "--help" => {
                println!("Usage: symhow notes accept -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]");
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                eprintln!("symhow notes accept: unknown option '{}'", arg);
                return 1;
            }
            _ => {
                if have_symbol {
                    eprintln!("symhow notes accept: unexpected extra argument '{}'", arg);
                    return 1;
                }
                symbol_spec = arg.to_string();
                have_symbol = true;
            }
        }
        i += 1;
    }

    if db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            db_path = env;
        } else {
            db_path = "./symhow.srctrldb".to_string();
        }
    }
    let selector_count =
        (all as i32) + (!file_glob.is_empty() as i32) + (have_symbol as i32);
    if selector_count != 1 {
        eprintln!("symhow notes accept: exactly one of SYMBOL / --all / --file GLOB is required");
        return 1;
    }

    let db = match Database::open(db_path.as_str(), 0) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow notes accept: {}", e.0);
            return 4;
        }
    };

    let notes_dir = if !notes_dir_flag.is_empty() {
        notes_dir_flag
    } else {
        resolve_notes_dir_default(&db_path)
    };
    if notes_dir.is_empty() {
        eprintln!("symhow notes accept: no notes dir given and none stored (nor a repo root) -- pass --notes-dir DIR");
        return 1;
    }

    let body = || -> Result<i32> {
        let lookup = open_notes(&db)?;

        // Determine which serialized_names to accept.
        let mut targets: HashSet<String> = HashSet::new();
        if all {
            for (sn, note) in &lookup.by_serialized_name {
                if note.stale {
                    targets.insert(sn.clone());
                }
            }
        } else if !file_glob.is_empty() {
            for (sn, note) in &lookup.by_serialized_name {
                if !note.stale {
                    continue;
                }
                let source_path = note.notes_path.clone();
                let source_path = if source_path.len() > 3 && source_path.ends_with(".md") {
                    source_path[..source_path.len() - 3].to_string()
                } else {
                    source_path
                };
                if glob::matches(&file_glob, &source_path) {
                    targets.insert(sn.clone());
                }
            }
        } else {
            let rr = resolve_symbol(&db, &symbol_spec, None, None)?;
            if rr.outcome == ResolveOutcome::Ambiguous {
                let engine = QueryEngine::new(&db)?;
                println!(
                    "symhow notes accept: '{}' is ambiguous ({} candidates):",
                    symbol_spec,
                    rr.candidates.len()
                );
                print!("{}", write_candidate_table(&to_candidate_rows(&engine, &rr.candidates)?));
                return Ok(2);
            }
            if rr.outcome == ResolveOutcome::NotFound {
                eprintln!("symhow notes accept: symbol '{}' not found.", symbol_spec);
                return Ok(3);
            }
            let sn = &rr.candidates[0].serialized_name;
            if !lookup.by_serialized_name.contains_key(sn) {
                eprintln!("symhow notes accept: symbol '{}' has no note.", symbol_spec);
                return Ok(3);
            }
            targets.insert(sn.clone());
        }

        if targets.is_empty() {
            if want_json {
                let j = serde_json::json!({
                    "symhow_version": symhow_version(),
                    "schema": "symhow.notes-accept/1",
                    "database": db_path,
                    "accepted": [],
                    "refused_files": [],
                });
                println!("{}", j.to_string());
            } else {
                println!("symhow notes accept: nothing to accept (0 stale/matching notes)");
            }
            return Ok(0);
        }

        // Group targets by notes file.
        let mut by_file: HashMap<String, Vec<String>> = HashMap::new();
        for sn in &targets {
            let notes_path = lookup
                .by_serialized_name
                .get(sn)
                .map(|n| n.notes_path.clone())
                .unwrap_or_default();
            by_file.entry(notes_path).or_default().push(sn.clone());
        }

        let commit = current_commit(&db);
        let mut accepted: Vec<String> = Vec::new();
        let mut accepted_display: Vec<String> = Vec::new();
        let mut refused_files: Vec<String> = Vec::new();

        for (notes_path, sns) in &by_file {
            let Some(content) = read_notes_file(&notes_dir, notes_path) else {
                refused_files.push(notes_path.clone());
                continue;
            };
            let mut parsed = parse_notes_file(&content, notes_path, false);
            let canonical = render_file(&parsed);
            if canonical != content {
                eprintln!(
                    "symhow notes accept: '{}' is not in canonical form; run `symhow notes fmt` first -- skipped",
                    notes_path
                );
                refused_files.push(notes_path.clone());
                continue;
            }

            let wanted: HashSet<&String> = sns.iter().collect();
            let mut any_changed = false;
            for block in &mut parsed.blocks {
                let mo: MatchOutcome = match_block(&db, block)?;
                if mo.kind != MatchKind::Matched || !wanted.contains(&mo.serialized_name) {
                    continue;
                }
                block.body_hash = body_hash_for_node(&db, mo.node_id)?;
                block.commit = commit.clone();
                accepted.push(mo.serialized_name.clone());
                accepted_display.push(mo.qualified_name.clone());
                any_changed = true;
            }
            if any_changed {
                write_notes_file(&notes_dir, notes_path, &render_file(&parsed));
            }
        }

        // Refresh the cache so `stale` clears for what was just accepted.
        if let Err(e) = sync_notes(&db_path, &notes_dir, false) {
            eprintln!("symhow notes accept: warning: post-accept sync failed: {}", e.0);
        }

        accepted.sort();
        accepted_display.sort();
        refused_files.sort();

        if want_json {
            let j = serde_json::json!({
                "symhow_version": symhow_version(),
                "schema": "symhow.notes-accept/1",
                "database": db_path,
                "accepted": accepted,
                "refused_files": refused_files,
            });
            println!("{}", j.to_string());
        } else {
            for qn in &accepted_display {
                println!("accepted: {}", qn);
            }
            println!(
                "{} accepted, {} file(s) refused (not canonical / unreadable)",
                accepted.len(),
                refused_files.len()
            );
        }
        Ok(if refused_files.is_empty() { 0 } else { 4 })
    };

    match body() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow notes accept: {}", e.0);
            1
        }
    }
}

// --- dispatch -----------------------------------------------------------------

const K_NOTES_USAGE: &str = "Usage: symhow notes VERB ...
  lint     --notes-dir DIR [--strict] [--json]
  sync     -d DB [--notes-dir DIR] [--strict] [--json]
  show     -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]
  status   -d DB [--stale] [--unfilled] [--unanchored] [--problems] [--json]
  symbols  -d DB [--kind K]... [--file GLOB] [--json]
  scaffold -d DB [--notes-dir DIR] (--all | --file GLOB | --class QNAME | SYMBOL)
                 [--kind K]... [--dry-run] [--json]
  accept   -d DB (SYMBOL | --all | --file GLOB) [--json]
  fmt      --notes-dir DIR [--check] [--json]
";

/// `symhow notes VERB ...` dispatch. Mirrors Cli.cpp's runNotes().
pub fn run_notes(args: &[String], start_idx: usize) -> i32 {
    if start_idx >= args.len() {
        eprint!("{}", K_NOTES_USAGE);
        return 1;
    }
    let verb = args[start_idx].as_str();
    match verb {
        "lint" => run_notes_lint(args, start_idx + 1),
        "sync" => run_notes_sync(args, start_idx + 1),
        "show" => run_notes_show(args, start_idx + 1),
        "status" => run_notes_status(args, start_idx + 1),
        "symbols" => run_notes_symbols(args, start_idx + 1),
        "scaffold" => run_notes_scaffold(args, start_idx + 1),
        "accept" => run_notes_accept(args, start_idx + 1),
        "fmt" => run_notes_fmt(args, start_idx + 1),
        "-h" | "--help" => {
            print!("{}", K_NOTES_USAGE);
            0
        }
        _ => {
            eprintln!("symhow notes: unknown verb '{}'", verb);
            1
        }
    }
}
