//! `symhow graph VERB ...` -- exposes `crate::graph`'s analyses (betweenness
//! centrality, personalized PageRank, dominator tree, backward slice,
//! weighted shortest path) through the CLI. See docs/GRAPH_SCHEMA_NOTES.md
//! for the schema study the graph layer follows.
//!
//! Rust-only subcommand: there is no C++ Cli.cpp counterpart to mirror, so
//! the flag parsing follows the same hand-rolled conventions the other verbs
//! use (exact messages, results to stdout, diagnostics to stderr, the shared
//! exit-code contract: 0 ok, 1 usage error, 2 ambiguous, 3 not found, 4
//! database error). Symbol arguments resolve through the same
//! `query::resolve_symbol` the query verbs use, with the same
//! ambiguous/not-found reporting (candidate table / did-you-mean
//! suggestions, or the symhow.ambiguous/1 / symhow.not-found/1 envelopes
//! under --json). JSON/DOT rendering lives in `out::graph_writer`.

use std::time::Instant;

use crate::cli::{symhow_version, try_parse_int_full};
use crate::common::{Error, Result};
use crate::db::enums::{node_kind, node_kind_to_string};
use crate::db::meta::get_meta;
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;
use crate::graph::{
    backward_slice, betweenness, dominator_tree, induced_subgraph_dot, load_call_graph,
    personalized_pagerank, resolve_entry_points, shortest_path, GraphLoadOptions, PageRankParams,
    PathWeight,
};
use crate::out::graph_writer::{
    centrality_json, dominator_tree_dot, dominators_json, pagerank_json, path_dot, path_json,
    slice_dot, slice_json, write_centrality, write_dominators, write_pagerank, write_path,
    write_slice,
};
use crate::out::json_writer::{JsonWriter, QueryContext};
use crate::out::text_writer::{write_candidate_table, CandidateRow};
use crate::query::{resolve_symbol, QueryEngine, ResolveOutcome};

const DEFAULT_TOP: usize = 25;

// --- shared helpers -------------------------------------------------------------

fn print_graph_usage_error(verb: &str, message: &str) {
    eprintln!("symhow graph {}: {}", verb, message);
    eprintln!("Try 'symhow graph --help' for usage.");
}

/// The value of a flag that requires one (already-verified present by the
/// caller's match arm ordering; kept symmetrical with query_cmd's helper).
fn flag_value(args: &[String], i: usize, verb: &str, flag: &str) -> Option<String> {
    if i + 1 >= args.len() {
        print_graph_usage_error(verb, &format!("option '{}' requires a value", flag));
        None
    } else {
        Some(args[i + 1].clone())
    }
}

/// Parses a `--top N` value: a whole non-negative integer (0 = all).
fn parse_top(args: &[String], i: usize, verb: &str) -> Option<usize> {
    let value = flag_value(args, i, verb, "--top")?;
    match try_parse_int_full(&value) {
        Some(n) if n >= 0 => Some(n as usize),
        _ => {
            print_graph_usage_error(
                verb,
                &format!(
                    "option '--top' expects a non-negative integer, got '{}'",
                    value
                ),
            );
            None
        }
    }
}

/// -d/--db fallback chain shared with the other DB verbs: the given value,
/// else $SYMHOW_DB, else ./symhow.srctrldb.
fn resolve_db_path(given: &str) -> String {
    if !given.is_empty() {
        return given.to_string();
    }
    if let Ok(env) = std::env::var("SYMHOW_DB") {
        return env;
    }
    "./symhow.srctrldb".to_string()
}

/// Opens the read-only database (exit 4 on failure, like the query verbs)
/// and prints the shared "index incomplete" notice unless --json/--quiet.
fn open_db(verb: &str, db_path: &str, verbosity: i32, json: bool, quiet: bool) -> Option<Database> {
    match Database::open(db_path, verbosity) {
        Ok(db) => {
            if !json && !quiet {
                let in_progress = get_meta(db.conn(), "symhow_index_in_progress");
                if in_progress.as_deref() == Some("1") {
                    eprintln!(
                        "symhow graph {}: index incomplete (interrupted) -- run `symhow update` to finish",
                        verb
                    );
                }
            }
            Some(db)
        }
        Err(e) => {
            eprintln!("symhow graph {}: {}", verb, e.0);
            None
        }
    }
}

fn millis_since(t0: Instant) -> i64 {
    t0.elapsed().as_millis() as i64
}

/// How many ranked rows to show for an effective `--top` (0 = all).
fn shown_count(top: usize, total: usize) -> usize {
    if top == 0 {
        total
    } else {
        top.min(total)
    }
}

/// Writes a `--dot FILE` payload, creating parent directories (the out layer
/// has no file writer of its own; the notes verbs' writers do exactly this).
fn write_dot(path: &str, dot: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::new(format!(
                    "cannot create parent directory of '{}': {}",
                    path, e
                ))
            })?;
        }
    }
    std::fs::write(path, dot).map_err(|e| Error::new(format!("cannot write '{}': {}", path, e)))
}

/// The candidate table rows for an ambiguous symbol report (same shape the
/// query verbs render; local copy -- the query_cmd one is private to it,
/// exactly like notes_cmd's).
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

/// Resolves one symbol spec through the query layer's `resolve_symbol` (the
/// same fuzzy/qualified-name/`#id` resolution `symhow query` uses) and
/// checks that the result participates in the loaded call graph. On success
/// returns `(node id, None)`; on a reported ambiguity / not-found /
/// not-a-participant failure returns `(0, Some(exit_code))` having already
/// printed the report (candidate table / did-you-mean suggestions, or the
/// symhow.ambiguous/1 / symhow.not-found/1 envelopes under --json).
fn resolve_graph_symbol(
    db: &Database,
    engine: &QueryEngine,
    json_writer: Option<&JsonWriter>,
    verb: &str,
    db_path: &str,
    flag_label: &str,
    spec: &str,
    graph: &crate::graph::CallGraph,
) -> Result<(i64, Option<i32>)> {
    let json_ctx = || QueryContext {
        verb: verb.to_string(),
        db: db_path.to_string(),
        symbol_or_text_or_path: spec.to_string(),
        ..Default::default()
    };

    let rr = resolve_symbol(db, spec, None, None)?;

    if rr.outcome == ResolveOutcome::Ambiguous {
        if let Some(writer) = json_writer {
            println!(
                "{}",
                writer.ambiguous(&json_ctx(), &rr.candidates)?.to_string()
            );
        } else {
            println!(
                "symhow graph {}: {} '{}' is ambiguous ({} candidates):",
                verb,
                flag_label,
                spec,
                rr.candidates.len()
            );
            print!(
                "{}",
                write_candidate_table(&to_candidate_rows(engine, &rr.candidates)?)
            );
            println!("Use '#<id>' (or a qualified name) to pick one directly.");
        }
        return Ok((0, Some(2)));
    }

    if rr.outcome == ResolveOutcome::NotFound {
        let hits = engine.search(spec, None, 5)?;
        if let Some(writer) = json_writer {
            println!("{}", writer.not_found(&json_ctx(), &hits)?.to_string());
        } else {
            eprintln!(
                "symhow graph {}: {} '{}': symbol not found.",
                verb, flag_label, spec
            );
            if !hits.is_empty() {
                eprintln!("did you mean:");
                for h in &hits {
                    eprintln!(
                        "  {} (#{}, {})",
                        h.display_name,
                        h.node_id,
                        node_kind_to_string(h.kind)
                    );
                }
            }
        }
        return Ok((0, Some(3)));
    }

    // Unique: the candidate must participate in the loaded graph.
    let candidate = &rr.candidates[0];
    if graph.index_of.contains_key(&candidate.id) {
        return Ok((candidate.id, None));
    }
    let hub_hint = if candidate.kind == node_kind::UNKNOWN {
        " -- the unsolved-symbol hub node is excluded from the graph by default"
    } else {
        ""
    };
    eprintln!(
        "symhow graph {}: {} '{}' resolves to {} (#{}, {}) which is not a participant of the loaded call graph{}",
        verb,
        flag_label,
        spec,
        decode_serialized_name(&candidate.serialized_name).display_name(),
        candidate.id,
        node_kind_to_string(candidate.kind),
        hub_hint
    );
    Ok((0, Some(3)))
}

// --- verb: centrality -----------------------------------------------------------

fn run_graph_centrality(args: &[String], start_idx: usize) -> i32 {
    let verb = "centrality";
    let mut db_path = String::new();
    let mut json = false;
    let mut dot: Option<String> = None;
    let mut verbosity = 0i32;
    let mut quiet = false;
    let mut top = DEFAULT_TOP;
    let mut include_unsolved = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    db_path = v;
                    i += 1;
                }
                None => return 1,
            },
            "--top" => match parse_top(args, i, verb) {
                Some(t) => {
                    top = t;
                    i += 1;
                }
                None => return 1,
            },
            "--include-unsolved" => include_unsolved = true,
            "--json" => json = true,
            "--dot" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    dot = Some(v);
                    i += 1;
                }
                None => return 1,
            },
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_verb_help(verb);
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                print_graph_usage_error(verb, &format!("unknown option '{}'", arg));
                return 1;
            }
            _ => {
                print_graph_usage_error(verb, &format!("unexpected argument '{}'", arg));
                return 1;
            }
        }
        i += 1;
    }

    let db_path = resolve_db_path(&db_path);
    let Some(db) = open_db(verb, &db_path, verbosity, json, quiet) else {
        return 4;
    };
    match centrality_body(
        &db,
        verb,
        &db_path,
        json,
        dot.as_deref(),
        top,
        include_unsolved,
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow graph {}: error: {}", verb, e.0);
            1
        }
    }
}

fn centrality_body(
    db: &Database,
    _verb: &str, // unused: centrality takes no symbol args, so it never reports per-symbol errors itself
    db_path: &str,
    json: bool,
    dot_path: Option<&str>,
    top: usize,
    include_unsolved: bool,
) -> Result<i32> {
    let options = GraphLoadOptions {
        include_usage: true,
        with_body_lines: false,
        exclude_unsolved: !include_unsolved,
    };
    let graph = load_call_graph(db, &options)?;

    let t0 = Instant::now();
    let ranked = betweenness(&graph)?;
    let elapsed_ms = millis_since(t0);
    let shown = shown_count(top, ranked.len());

    if json {
        let j = centrality_json(
            symhow_version(),
            db_path,
            top,
            include_unsolved,
            &graph,
            &ranked,
            shown,
            elapsed_ms,
        );
        println!("{}", j.to_string());
    } else {
        print!("{}", write_centrality(&graph, &ranked, shown));
    }
    if let Some(dot_path) = dot_path {
        let node_ids: Vec<i64> = ranked.iter().take(shown).map(|n| n.node_id).collect();
        let dot = induced_subgraph_dot(&graph, &node_ids)?;
        write_dot(dot_path, &dot)?;
    }
    Ok(0)
}

// --- verb: pagerank ---------------------------------------------------------------

fn run_graph_pagerank(args: &[String], start_idx: usize) -> i32 {
    let verb = "pagerank";
    let mut db_path = String::new();
    let mut json = false;
    let mut dot: Option<String> = None;
    let mut verbosity = 0i32;
    let mut quiet = false;
    let mut top = DEFAULT_TOP;
    let mut seeds: Vec<String> = Vec::new();
    let mut damping: Option<f64> = None;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    db_path = v;
                    i += 1;
                }
                None => return 1,
            },
            "--seed" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    seeds.push(v);
                    i += 1;
                }
                None => return 1,
            },
            "--damping" => match flag_value(args, i, verb, arg) {
                Some(v) => match v.parse::<f64>() {
                    Ok(d) => {
                        damping = Some(d);
                        i += 1;
                    }
                    Err(_) => {
                        print_graph_usage_error(
                            verb,
                            &format!("option '--damping' expects a number, got '{}'", v),
                        );
                        return 1;
                    }
                },
                None => return 1,
            },
            "--top" => match parse_top(args, i, verb) {
                Some(t) => {
                    top = t;
                    i += 1;
                }
                None => return 1,
            },
            "--json" => json = true,
            "--dot" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    dot = Some(v);
                    i += 1;
                }
                None => return 1,
            },
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_verb_help(verb);
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                print_graph_usage_error(verb, &format!("unknown option '{}'", arg));
                return 1;
            }
            _ => {
                print_graph_usage_error(verb, &format!("unexpected argument '{}'", arg));
                return 1;
            }
        }
        i += 1;
    }

    let db_path = resolve_db_path(&db_path);
    let Some(db) = open_db(verb, &db_path, verbosity, json, quiet) else {
        return 4;
    };
    match pagerank_body(
        &db,
        verb,
        &db_path,
        json,
        dot.as_deref(),
        top,
        &seeds,
        damping,
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow graph {}: error: {}", verb, e.0);
            1
        }
    }
}

fn pagerank_body(
    db: &Database,
    verb: &str,
    db_path: &str,
    json: bool,
    dot_path: Option<&str>,
    top: usize,
    seeds: &[String],
    damping: Option<f64>,
) -> Result<i32> {
    let graph = load_call_graph(db, &GraphLoadOptions::default())?;
    let engine = QueryEngine::new(db)?;
    let json_writer = if json {
        Some(JsonWriter::new(db, &engine, symhow_version()))
    } else {
        None
    };

    // Seeds: explicit --seed specs (strict per-spec resolution, deduped), or
    // the entry-point fallback (`main` heuristic, then root-file functions).
    let (seed_ids, seeds_explicit) = if seeds.is_empty() {
        let ids = resolve_entry_points(db, &graph, &[])?;
        if ids.is_empty() {
            eprintln!(
                "symhow graph pagerank: no entry points found to seed PageRank (no `main`, no root-file functions) -- pass --seed SYMBOL"
            );
            return Ok(3);
        }
        (ids, false)
    } else {
        let mut ids = std::collections::BTreeSet::new();
        for spec in seeds {
            let (id, code) = resolve_graph_symbol(
                db,
                &engine,
                json_writer.as_ref(),
                verb,
                db_path,
                "--seed",
                spec,
                &graph,
            )?;
            if let Some(code) = code {
                return Ok(code);
            }
            ids.insert(id);
        }
        (ids.into_iter().collect(), true)
    };

    let params = PageRankParams {
        damping: damping.unwrap_or(0.85),
        ..Default::default()
    };

    let t0 = Instant::now();
    let ranked = personalized_pagerank(&graph, &seed_ids, &params)?;
    let elapsed_ms = millis_since(t0);
    let shown = shown_count(top, ranked.len());

    if json {
        let j = pagerank_json(
            symhow_version(),
            db_path,
            seeds,
            &seed_ids,
            seeds_explicit,
            params.damping,
            top,
            &graph,
            &ranked,
            shown,
            elapsed_ms,
        );
        println!("{}", j.to_string());
    } else {
        print!(
            "{}",
            write_pagerank(&graph, &ranked, shown, params.damping, seed_ids.len())
        );
    }
    if let Some(dot_path) = dot_path {
        let node_ids: Vec<i64> = ranked.iter().take(shown).map(|n| n.node_id).collect();
        let dot = induced_subgraph_dot(&graph, &node_ids)?;
        write_dot(dot_path, &dot)?;
    }
    Ok(0)
}

// --- verb: dominators --------------------------------------------------------------

fn run_graph_dominators(args: &[String], start_idx: usize) -> i32 {
    let verb = "dominators";
    let mut db_path = String::new();
    let mut json = false;
    let mut dot: Option<String> = None;
    let mut verbosity = 0i32;
    let mut quiet = false;
    let mut entry = String::new();
    let mut have_entry = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    db_path = v;
                    i += 1;
                }
                None => return 1,
            },
            "--entry" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    entry = v;
                    have_entry = true;
                    i += 1;
                }
                None => return 1,
            },
            "--json" => json = true,
            "--dot" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    dot = Some(v);
                    i += 1;
                }
                None => return 1,
            },
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_verb_help(verb);
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                print_graph_usage_error(verb, &format!("unknown option '{}'", arg));
                return 1;
            }
            _ => {
                print_graph_usage_error(verb, &format!("unexpected argument '{}'", arg));
                return 1;
            }
        }
        i += 1;
    }

    let db_path = resolve_db_path(&db_path);
    let Some(db) = open_db(verb, &db_path, verbosity, json, quiet) else {
        return 4;
    };
    match dominators_body(
        &db,
        verb,
        &db_path,
        json,
        dot.as_deref(),
        &entry,
        have_entry,
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow graph {}: error: {}", verb, e.0);
            1
        }
    }
}

fn dominators_body(
    db: &Database,
    verb: &str,
    db_path: &str,
    json: bool,
    dot_path: Option<&str>,
    entry: &str,
    have_entry: bool,
) -> Result<i32> {
    let graph = load_call_graph(db, &GraphLoadOptions::default())?;
    let engine = QueryEngine::new(db)?;
    let json_writer = if json {
        Some(JsonWriter::new(db, &engine, symhow_version()))
    } else {
        None
    };

    // The root: the explicit --entry symbol, or the entry-point fallback.
    // The fallback's `main` heuristic can legitimately find several
    // TU-local mains -- that is an ambiguity the caller must resolve, so
    // list the candidates and exit 2.
    let (root, entry_explicit) = if have_entry {
        let (id, code) = resolve_graph_symbol(
            db,
            &engine,
            json_writer.as_ref(),
            verb,
            db_path,
            "--entry",
            entry,
            &graph,
        )?;
        if let Some(code) = code {
            return Ok(code);
        }
        (id, true)
    } else {
        match resolve_entry_points(db, &graph, &[])?.as_slice() {
            [] => {
                eprintln!(
                    "symhow graph dominators: no entry point found (no `main`, no root-file functions) -- pass --entry SYMBOL"
                );
                return Ok(3);
            }
            [only] => (*only, false),
            many => {
                eprintln!(
                    "symhow graph dominators: {} entry-point candidates and no --entry given; pass --entry SYMBOL or --entry '#<id>':",
                    many.len()
                );
                for &id in many {
                    eprintln!(
                        "  #{:<6}  {}",
                        id,
                        graph
                            .nodes
                            .get(&id)
                            .map(|node| node.qualified_name.clone())
                            .unwrap_or_default()
                    );
                }
                return Ok(2);
            }
        }
    };

    let t0 = Instant::now();
    let tree = dominator_tree(&graph, root)?;
    let elapsed_ms = millis_since(t0);

    if json {
        let j = dominators_json(
            symhow_version(),
            db_path,
            if entry_explicit { entry } else { "" },
            root,
            entry_explicit,
            &graph,
            &tree,
            elapsed_ms,
        );
        println!("{}", j.to_string());
    } else {
        print!("{}", write_dominators(&graph, &tree));
    }
    if let Some(dot_path) = dot_path {
        let dot = dominator_tree_dot(&graph, &tree);
        write_dot(dot_path, &dot)?;
    }
    Ok(0)
}

// --- verb: slice -------------------------------------------------------------------

fn run_graph_slice(args: &[String], start_idx: usize) -> i32 {
    let verb = "slice";
    let mut db_path = String::new();
    let mut json = false;
    let mut dot: Option<String> = None;
    let mut verbosity = 0i32;
    let mut quiet = false;
    let mut criterion = String::new();
    let mut have_criterion = false;
    let mut show_lines = false;

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    db_path = v;
                    i += 1;
                }
                None => return 1,
            },
            "--criterion" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    criterion = v;
                    have_criterion = true;
                    i += 1;
                }
                None => return 1,
            },
            "--lines" => show_lines = true,
            "--json" => json = true,
            "--dot" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    dot = Some(v);
                    i += 1;
                }
                None => return 1,
            },
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_verb_help(verb);
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                print_graph_usage_error(verb, &format!("unknown option '{}'", arg));
                return 1;
            }
            _ => {
                print_graph_usage_error(verb, &format!("unexpected argument '{}'", arg));
                return 1;
            }
        }
        i += 1;
    }

    if !have_criterion {
        print_graph_usage_error(verb, "missing required option --criterion SYMBOL");
        return 1;
    }

    let db_path = resolve_db_path(&db_path);
    let Some(db) = open_db(verb, &db_path, verbosity, json, quiet) else {
        return 4;
    };
    match slice_body(
        &db,
        verb,
        &db_path,
        json,
        dot.as_deref(),
        &criterion,
        show_lines,
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow graph {}: error: {}", verb, e.0);
            1
        }
    }
}

fn slice_body(
    db: &Database,
    verb: &str,
    db_path: &str,
    json: bool,
    dot_path: Option<&str>,
    criterion: &str,
    show_lines: bool,
) -> Result<i32> {
    // Slicing needs the USAGE edges (with their source-line sites) -- the
    // loader default; without them the slice degrades to CALL edges only.
    let graph = load_call_graph(db, &GraphLoadOptions::default())?;
    let engine = QueryEngine::new(db)?;
    let json_writer = if json {
        Some(JsonWriter::new(db, &engine, symhow_version()))
    } else {
        None
    };

    let (criterion_id, code) = resolve_graph_symbol(
        db,
        &engine,
        json_writer.as_ref(),
        verb,
        db_path,
        "--criterion",
        criterion,
        &graph,
    )?;
    if let Some(code) = code {
        return Ok(code);
    }

    let t0 = Instant::now();
    let slice = backward_slice(&graph, criterion_id)?;
    let elapsed_ms = millis_since(t0);

    if json {
        let j = slice_json(
            symhow_version(),
            db_path,
            criterion,
            criterion_id,
            show_lines,
            &graph,
            &slice,
            elapsed_ms,
        );
        println!("{}", j.to_string());
    } else {
        print!("{}", write_slice(&graph, &slice, show_lines));
    }
    if let Some(dot_path) = dot_path {
        let dot = slice_dot(&graph, &slice, show_lines);
        write_dot(dot_path, &dot)?;
    }
    Ok(0)
}

// --- verb: path ---------------------------------------------------------------------

fn run_graph_path(args: &[String], start_idx: usize) -> i32 {
    let verb = "path";
    let mut db_path = String::new();
    let mut json = false;
    let mut dot: Option<String> = None;
    let mut verbosity = 0i32;
    let mut quiet = false;
    let mut from = String::new();
    let mut have_from = false;
    let mut to = String::new();
    let mut have_to = false;
    let mut weight = PathWeight::default();

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    db_path = v;
                    i += 1;
                }
                None => return 1,
            },
            "--from" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    from = v;
                    have_from = true;
                    i += 1;
                }
                None => return 1,
            },
            "--to" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    to = v;
                    have_to = true;
                    i += 1;
                }
                None => return 1,
            },
            "--weight" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    weight = match v.as_str() {
                        "hops" => PathWeight::Hops,
                        "calls" => PathWeight::Calls,
                        "lines" => PathWeight::Lines,
                        other => {
                            print_graph_usage_error(
                                verb,
                                &format!(
                                    "unknown --weight value '{}' (expected hops|calls|lines)",
                                    other
                                ),
                            );
                            return 1;
                        }
                    };
                    i += 1;
                }
                None => return 1,
            },
            "--json" => json = true,
            "--dot" => match flag_value(args, i, verb, arg) {
                Some(v) => {
                    dot = Some(v);
                    i += 1;
                }
                None => return 1,
            },
            "-v" | "--verbose" => verbosity += 1,
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_verb_help(verb);
                return 0;
            }
            _ if !arg.is_empty() && arg.starts_with('-') && arg != "-" => {
                print_graph_usage_error(verb, &format!("unknown option '{}'", arg));
                return 1;
            }
            _ => {
                print_graph_usage_error(verb, &format!("unexpected argument '{}'", arg));
                return 1;
            }
        }
        i += 1;
    }

    if !have_from {
        print_graph_usage_error(verb, "missing required option --from SYMBOL");
        return 1;
    }
    if !have_to {
        print_graph_usage_error(verb, "missing required option --to SYMBOL");
        return 1;
    }

    let db_path = resolve_db_path(&db_path);
    let Some(db) = open_db(verb, &db_path, verbosity, json, quiet) else {
        return 4;
    };
    match path_body(
        &db,
        verb,
        &db_path,
        json,
        dot.as_deref(),
        &from,
        &to,
        weight,
    ) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow graph {}: error: {}", verb, e.0);
            1
        }
    }
}

fn path_body(
    db: &Database,
    verb: &str,
    db_path: &str,
    json: bool,
    dot_path: Option<&str>,
    from: &str,
    to: &str,
    weight: PathWeight,
) -> Result<i32> {
    // The `lines` weight needs the per-function body-line sizes loaded.
    let options = GraphLoadOptions {
        include_usage: true,
        with_body_lines: weight == PathWeight::Lines,
        exclude_unsolved: true,
    };
    let graph = load_call_graph(db, &options)?;
    let engine = QueryEngine::new(db)?;
    let json_writer = if json {
        Some(JsonWriter::new(db, &engine, symhow_version()))
    } else {
        None
    };

    let (from_id, code) = resolve_graph_symbol(
        db,
        &engine,
        json_writer.as_ref(),
        verb,
        db_path,
        "--from",
        from,
        &graph,
    )?;
    if let Some(code) = code {
        return Ok(code);
    }
    let (to_id, code) = resolve_graph_symbol(
        db,
        &engine,
        json_writer.as_ref(),
        verb,
        db_path,
        "--to",
        to,
        &graph,
    )?;
    if let Some(code) = code {
        return Ok(code);
    }

    let t0 = Instant::now();
    let path = shortest_path(&graph, from_id, to_id, weight)?;
    let elapsed_ms = millis_since(t0);

    if json {
        let j = path_json(
            symhow_version(),
            db_path,
            from,
            from_id,
            to,
            to_id,
            weight,
            &graph,
            path.as_ref(),
            elapsed_ms,
        );
        println!("{}", j.to_string());
    } else {
        print!("{}", write_path(&graph, from_id, to_id, path.as_ref()));
    }
    if let Some(dot_path) = dot_path {
        let dot = path_dot(&graph, path.as_ref());
        write_dot(dot_path, &dot)?;
    }
    Ok(0)
}

// --- help -----------------------------------------------------------------------

/// `symhow graph --help`: the complete, self-explanatory subcommand help.
pub fn print_graph_help() {
    print!(
        r#"Usage: symhow graph VERB -d DB [OPTIONS]

Graph analysis over the indexed call/usage graph (Rust-only subcommand; see
docs/GRAPH_SCHEMA_NOTES.md). The graph is loaded per invocation: every
FUNCTION and METHOD node plus every CALL edge, with USAGE edges (field/global
accesses, carrying their source-line sites) on top, and the shared
unsolved-symbol hub node excluded (`centrality --include-unsolved` re-includes
it). Symbols resolve exactly like `symhow query`: qualified/display/leaf name,
or '#<id>' for a direct node id.

VERB is one of: centrality pagerank dominators slice path

Shared options (all verbs):
  -d, --db PATH         database to query (default: $SYMHOW_DB or ./symhow.srctrldb)
      --json            print machine-readable JSON instead of text
                        (schemas: symhow.graph.centrality/1, symhow.graph.pagerank/1,
                        symhow.graph.dominators/1, symhow.graph.slice/1,
                        symhow.graph.path/1)
      --dot FILE        also write a Graphviz DOT rendering to FILE (parent
                        directories are created). centrality/pagerank render the
                        subgraph induced by the reported (top-N) nodes; the other
                        verbs render their own result structure.
  -v, --verbose         increase verbosity (repeatable)
  -q, --quiet           suppress the "index incomplete (interrupted)" notice
  -h, --help            show this help (per verb: `symhow graph VERB --help`)

centrality -- betweenness centrality ranking (Brandes, unweighted; scores are
raw dependency sums, not halved, not normalized -- normalized value =
score / ((n-1)(n-2))):
      --top N              show the top N nodes (default: 25; 0 = all)
      --include-unsolved   keep the unsolved-symbol hub node in the graph

pagerank -- personalized PageRank (power iteration; scores sum to ~1):
      --seed SYMBOL        entry-point seed, repeatable (default: the `main`
                           heuristic, then root-file functions)
      --damping F          damping factor in [0,1] (default: 0.85)
      --top N              show the top N nodes (default: 25; 0 = all)

dominators -- dominator tree of the call graph from one entry point (which
functions must have been called on every path from the entry to a node):
      --entry SYMBOL       entry point (default: the `main` heuristic; when
                           several candidates exist they are listed and one
                           must be picked with --entry)

slice -- backward slice of one criterion (reverse transitive closure over
USAGE+CALL edges, function granularity; approximate -- USAGE does not
distinguish reads from writes):
      --criterion SYMBOL   slice criterion (required)
      --lines              show per-edge source-line sites

path -- weighted shortest path between two symbols:
      --from SYMBOL        source (required)
      --to SYMBOL          target (required)
      --weight MODE        hops | calls | lines (default: hops; `calls` weights
                           an edge by its call-site count, `lines` by the
                           callee's body line count). An unreachable target is
                           reported as "no path" (exit 0), not an error.

Exit codes: 0 ok, 1 usage error, 2 ambiguous symbol / entry-point choice,
            3 not found (symbol or entry point), 4 database error.
"#
    );
}

/// Per-verb `--help` (one usage line + the verb's specifics; the shared
/// options and JSON schema names live in `symhow graph --help`).
fn print_verb_help(verb: &str) {
    let text = match verb {
        "centrality" => "\
Usage: symhow graph centrality [-d DB] [--top N] [--include-unsolved] [--json] [--dot FILE]

Betweenness centrality over the loaded call/usage graph (Brandes, unweighted).
Scores are raw dependency sums -- normalized value = score / ((n-1)(n-2)).
Ranked by score descending; --top caps both the listing and the --dot subgraph.
--include-unsolved keeps the shared unsolved-symbol hub node in the graph
(excluded by default because it collects every unresolved reference).

See 'symhow graph --help' for the shared options and the JSON schema names.
",
        "pagerank" => "\
Usage: symhow graph pagerank [-d DB] [--seed SYMBOL]... [--damping F] [--top N] [--json] [--dot FILE]

Personalized PageRank over the loaded call/usage graph (power iteration,
scores sum to ~1). Seeds are the resolved --entry-style symbols (repeatable
--seed, every candidate of an unambiguous name counts); with no --seed the
entry-point fallback applies: FUNCTION nodes named `main`, then root-file
functions. --damping is the damping factor in [0,1] (default 0.85).

See 'symhow graph --help' for the shared options and the JSON schema names.
",
        "dominators" => "\
Usage: symhow graph dominators [-d DB] [--entry SYMBOL] [--json] [--dot FILE]

Dominator tree of the call graph rooted at one entry point: which functions
must have been called on every path from the entry to a node (call-graph
dominance, not control-flow dominance -- the schema has no basic blocks).
--entry defaults to the `main` heuristic; when several candidates exist
(e.g. TU-local mains) they are listed and one must be picked.

See 'symhow graph --help' for the shared options and the JSON schema names.
",
        "slice" => "\
Usage: symhow graph slice -d DB --criterion SYMBOL [--lines] [--json] [--dot FILE]

Backward slice of the criterion symbol: its reverse transitive closure over
the USAGE+CALL edges (function granularity), with the induced dependence
edges between members. --lines annotates each edge with its source-line
sites. Approximate by design: USAGE does not distinguish reads from writes,
and there is no statement-level dependence.

See 'symhow graph --help' for the shared options and the JSON schema names.
",
        "path" => "\
Usage: symhow graph path -d DB --from SYMBOL --to SYMBOL [--weight hops|calls|lines] [--json] [--dot FILE]

Weighted shortest path between two symbols (Dijkstra over the loaded
call/usage graph). Weight schemes: hops (uniform 1 per edge), calls (the
edge's call-site count), lines (the callee's body line count). An unreachable
target prints 'no path' and exits 0 -- reachability is the answer, not an
error.

See 'symhow graph --help' for the shared options and the JSON schema names.
",
        _ => return,
    };
    print!("{}", text);
}

// --- dispatch --------------------------------------------------------------------

/// `symhow graph VERB ...`: argv[start_idx] is the verb.
pub fn run_graph_cmd(args: &[String], start_idx: usize) -> i32 {
    const KNOWN_VERBS: [&str; 5] = ["centrality", "pagerank", "dominators", "slice", "path"];

    if start_idx >= args.len() {
        eprintln!(
            "symhow graph: missing VERB (expected one of: centrality pagerank dominators slice path)"
        );
        eprintln!("Try 'symhow graph --help' for usage.");
        return 1;
    }

    let verb = args[start_idx].as_str();
    if verb == "-h" || verb == "--help" {
        print_graph_help();
        return 0;
    }
    if !KNOWN_VERBS.contains(&verb) {
        eprintln!("symhow graph: unknown verb '{}'", verb);
        eprintln!("Try 'symhow graph --help' for usage.");
        return 1;
    }

    match verb {
        "centrality" => run_graph_centrality(args, start_idx + 1),
        "pagerank" => run_graph_pagerank(args, start_idx + 1),
        "dominators" => run_graph_dominators(args, start_idx + 1),
        "slice" => run_graph_slice(args, start_idx + 1),
        "path" => run_graph_path(args, start_idx + 1),
        _ => unreachable!("checked against KNOWN_VERBS above"),
    }
}
