//! `symhow query VERB ...` (Cli.cpp's runQuery): flag parsing, symbol
//! resolution with the ambiguous/not-found contract, and dispatch to
//! `symhow::query::QueryEngine`, rendering via `symhow::out` text/JSON
//! writers.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::cli::{print_help, symhow_version, try_parse_int_full};
use crate::common::Result;
use crate::db::enums::{
    edge_kind, node_kind, edge_kind_from_string, location_kind_from_string, node_kind_from_string,
    node_kind_to_string,
};
use crate::db::meta::get_meta;
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;
use crate::out::json_writer::{decompose_edge_kind_mask, JsonWriter, QueryContext};
use crate::out::text_writer::{
    write_bfs_result, write_candidate_table, write_definition, write_file_result,
    write_references, write_search_hits, write_stats, CandidateRow, NodeNoteText,
};
use crate::query::{
    fuzzy_match, resolve_file_path, resolve_symbol, QueryEngine, ResolveOutcome, ResolvedSymbol,
};
use crate::notes::{open_notes, NoteBlock, ListSection, TextSection};
use crate::notes::markdown::render_block;

fn print_query_usage_error(message: &str) {
    eprintln!("symhow query: {}", message);
    eprintln!("Try 'symhow --help' for usage.");
}

#[derive(Debug, Clone, Default)]
struct QueryFlags {
    db_path: String,
    depth: i32,
    max_nodes: i32,
    limit: i32,
    kind_names: Vec<String>,
    edge_kind_names: Vec<String>,
    location_kind_names: Vec<String>,
    all_matches: bool,
    has_signature: bool,
    signature: String,
    ensure_indices: bool,
    want_contents: bool,
    want_occurrences: bool,
    has_line_range: bool,
    line_a: i32,
    line_b: i32,
    errors_limit: i32,
    verbosity: i32,
    json: bool,
    quiet: bool,
    no_notes: bool,   // `symhow query context`: omit the "notes" field entirely
    notes_full: bool, // `symhow query context` text mode: print the full note block
}

impl QueryFlags {
    fn new() -> Self {
        QueryFlags {
            depth: 1,
            max_nodes: 500,
            limit: 100,
            errors_limit: 100,
            ..Default::default()
        }
    }
}

/// Parses everything after the verb into `flags` and (if present) the single
/// SYMBOL/TEXT/PATH positional argument. Returns None (having already
/// printed a usage error) on any parse failure. Mirrors Cli.cpp's
/// parseQueryFlags().
fn parse_query_flags(
    args: &[String],
    start_idx: usize,
    flags: &mut QueryFlags,
    positional: &mut String,
    have_positional: &mut bool,
) -> bool {
    *have_positional = false;

    let require_value = |i: usize, flag: &str| -> Option<String> {
        if i + 1 >= args.len() {
            print_query_usage_error(&format!("option '{}' requires a value", flag));
            return None;
        }
        Some(args[i + 1].clone())
    };
    let require_int_value = |i: usize, flag: &str, out: &mut i32| -> Option<()> {
        let v = require_value(i, flag)?;
        match try_parse_int_full(&v) {
            Some(parsed) => {
                *out = parsed;
                Some(())
            }
            None => {
                print_query_usage_error(&format!("option '{}' expects an integer, got '{}'", flag, v));
                None
            }
        }
    };

    let mut i = start_idx;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-d" | "--db" => {
                flags.db_path = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
            }
            "--depth" => {
                if require_int_value(i, arg, &mut flags.depth).is_none() {
                    return false;
                }
                i += 1;
            }
            "--max-nodes" => {
                if require_int_value(i, arg, &mut flags.max_nodes).is_none() {
                    return false;
                }
                i += 1;
            }
            "--kind" => {
                flags.kind_names.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
            }
            "--edge-kind" => {
                flags.edge_kind_names.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
            }
            "--location-kind" => {
                flags.location_kind_names.push(match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                });
                i += 1;
            }
            "--limit" => {
                if require_int_value(i, arg, &mut flags.limit).is_none() {
                    return false;
                }
                i += 1;
            }
            "--all-matches" => {
                flags.all_matches = true;
            }
            "--signature" => {
                flags.signature = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                flags.has_signature = true;
                i += 1;
            }
            "--ensure-indices" => {
                flags.ensure_indices = true;
            }
            "--contents" => {
                flags.want_contents = true;
            }
            "--occurrences" => {
                flags.want_occurrences = true;
            }
            "--lines" => {
                let v = match require_value(i, arg) {
                    Some(v) => v,
                    None => return false,
                };
                i += 1;
                let colon = v.find(':');
                let mut ok = colon.is_some();
                if let Some(colon) = colon {
                    ok = try_parse_int_full(&v[..colon])
                        .map(|a| {
                            flags.line_a = a;
                            true
                        })
                        .unwrap_or(false)
                        && try_parse_int_full(&v[colon + 1..])
                            .map(|b| {
                                flags.line_b = b;
                                true
                            })
                            .unwrap_or(false);
                }
                if !ok {
                    print_query_usage_error(&format!("invalid --lines value '{}' (expected A:B)", v));
                    return false;
                }
                flags.has_line_range = true;
            }
            "--errors" => {
                if require_int_value(i, arg, &mut flags.errors_limit).is_none() {
                    return false;
                }
                i += 1;
            }
            "--json" => {
                flags.json = true;
            }
            "--no-notes" => {
                flags.no_notes = true;
            }
            "--notes-full" => {
                flags.notes_full = true;
            }
            "-v" | "--verbose" => {
                flags.verbosity += 1;
            }
            "-q" | "--quiet" => {
                flags.quiet = true;
            }
            "-h" | "--help" => {
                print_help("symhow");
                std::process::exit(0);
            }
            _ if !arg.is_empty() && arg.starts_with('-') => {
                print_query_usage_error(&format!("unknown option '{}'", arg));
                return false;
            }
            _ => {
                if *have_positional {
                    print_query_usage_error(&format!("unexpected extra argument '{}'", arg));
                    return false;
                }
                *positional = arg.to_string();
                *have_positional = true;
            }
        }
        // Arms that consumed a value already advanced i to it; this trailing
        // step moves past it (the C++ for-loop's ++i).
        i += 1;
    }
    true
}

/// Resolves `names` (raw --kind/--edge-kind/--location-kind flag values)
/// through `from_string`, appending to `out`. Prints a usage error and
/// returns false on the first unrecognized name.
fn resolve_kind_names(
    names: &[String],
    from_string: fn(&str) -> Option<i32>,
    flag_label: &str,
    out: &mut Vec<i32>,
) -> bool {
    for name in names {
        match from_string(name) {
            Some(value) => out.push(value),
            None => {
                print_query_usage_error(&format!("unknown {} value '{}'", flag_label, name));
                return false;
            }
        }
    }
    true
}

fn to_candidate_rows(
    engine: &QueryEngine,
    candidates: &[ResolvedSymbol],
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

/// Resolves `spec` to a set of BFS seeds per the CLI's ambiguous/not-found
/// contract. On success (Unique, or Ambiguous+--all-matches) returns the
/// seeds; otherwise prints the appropriate message/table (or, under --json,
/// the symhow.ambiguous/1 / symhow.not-found/1 envelope to stdout) and
/// returns the exit code the caller should return immediately (2 for
/// ambiguous, 3 for not found). Mirrors Cli.cpp's resolveSeedsOrReport().
#[allow(clippy::too_many_arguments)]
fn resolve_seeds_or_report(
    db: &Database,
    engine: &QueryEngine,
    json_writer: Option<&JsonWriter>,
    json_ctx: &QueryContext,
    spec: &str,
    kind_filter: Option<&[i32]>,
    signature_filter: Option<&str>,
    all_matches: bool,
) -> Result<(Vec<i64>, Option<i32>)> {
    let rr = resolve_symbol(db, spec, kind_filter, signature_filter)?;

    if rr.outcome == ResolveOutcome::Unique {
        return Ok((vec![rr.candidates[0].id], None));
    }

    if rr.outcome == ResolveOutcome::Ambiguous {
        if all_matches {
            return Ok((rr.candidates.iter().map(|c| c.id).collect(), None));
        }
        if let Some(writer) = json_writer {
            println!("{}", writer.ambiguous(json_ctx, &rr.candidates)?.to_string());
        } else {
            println!(
                "symhow query: '{}' is ambiguous ({} candidates):",
                spec,
                rr.candidates.len()
            );
            print!("{}", write_candidate_table(&to_candidate_rows(engine, &rr.candidates)?));
            println!("Pass --all-matches to query every candidate, or use '#<id>' to pick one directly.");
        }
        return Ok((Vec::new(), Some(2)));
    }

    // NotFound: fuzzy-suggest against every node's display name.
    let hits = engine.search(spec, None, 5)?;
    if let Some(writer) = json_writer {
        println!("{}", writer.not_found(json_ctx, &hits)?.to_string());
    } else {
        eprintln!("symhow query: symbol '{}' not found.", spec);
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
    Ok((Vec::new(), Some(3)))
}

/// Unions BFS results discovered by separate QueryEngine calls into one
/// BfsResult, for `symhow query context`. Nodes are deduped by id (first
/// BfsNode seen for an id wins); edges are deduped by id.
fn merge_bfs_results(parts: Vec<crate::query::BfsResult>) -> crate::query::BfsResult {
    let mut out = crate::query::BfsResult::default();
    let mut seen_nodes: HashSet<i64> = HashSet::new();
    let mut seen_edges: HashSet<i64> = HashSet::new();
    for part in parts {
        for n in part.nodes {
            if seen_nodes.insert(n.id) {
                out.nodes.push(n);
            }
        }
        for e in part.edges {
            if seen_edges.insert(e.id) {
                out.edges.push(e);
            }
        }
        out.truncated = out.truncated || part.truncated;
    }
    out
}

fn edge_kind_mask_or(kinds: &[i32], fallback: i32) -> i32 {
    if kinds.is_empty() {
        return fallback;
    }
    let mut mask = 0;
    for k in kinds {
        mask |= k;
    }
    mask
}

fn millis_since(t0: Instant) -> i64 {
    t0.elapsed().as_millis() as i64
}

/// Parses `symhow query` subcommand arguments (argv[start_idx..]) and
/// dispatches to QueryEngine, rendering results via the out layer. Mirrors
/// Cli.cpp's runQuery().
pub fn run_query_cmd(args: &[String], start_idx: usize) -> i32 {
    const KNOWN_VERBS: [&str; 14] = [
        "callers",
        "callees",
        "bases",
        "derived",
        "overrides",
        "overridden-by",
        "members",
        "neighbors",
        "references",
        "definition",
        "search",
        "file",
        "stats",
        "context",
    ];

    if start_idx >= args.len() {
        print_query_usage_error(
            "missing VERB (expected one of: callers callees bases derived overrides overridden-by \
             members neighbors references definition search file stats context)",
        );
        return 1;
    }

    let verb = args[start_idx].clone();
    if verb == "-h" || verb == "--help" {
        print_help("symhow");
        return 0;
    }
    if !KNOWN_VERBS.contains(&verb.as_str()) {
        print_query_usage_error(&format!("unknown verb '{}'", verb));
        return 1;
    }

    let mut flags = QueryFlags::new();
    let mut positional = String::new();
    let mut have_positional = false;
    if !parse_query_flags(args, start_idx + 1, &mut flags, &mut positional, &mut have_positional) {
        return 1;
    }

    let needs_positional = verb != "stats";
    if needs_positional && !have_positional {
        print_query_usage_error(&format!("verb '{}' requires a SYMBOL/TEXT/PATH argument", verb));
        return 1;
    }
    if !needs_positional && have_positional {
        print_query_usage_error("verb 'stats' does not take a positional argument");
        return 1;
    }

    if flags.db_path.is_empty() {
        if let Ok(env) = std::env::var("SYMHOW_DB") {
            flags.db_path = env;
        } else {
            flags.db_path = "./symhow.srctrldb".to_string();
        }
    }

    let mut kind_filter: Vec<i32> = Vec::new();
    let mut edge_kind_filter: Vec<i32> = Vec::new();
    let mut location_kind_filter: Vec<i32> = Vec::new();
    if !resolve_kind_names(&flags.kind_names, node_kind_from_string, "--kind", &mut kind_filter)
        || !resolve_kind_names(
            &flags.edge_kind_names,
            edge_kind_from_string,
            "--edge-kind",
            &mut edge_kind_filter,
        )
        || !resolve_kind_names(
            &flags.location_kind_names,
            location_kind_from_string,
            "--location-kind",
            &mut location_kind_filter,
        )
    {
        return 1;
    }

    // The C++ has ensureIndices() inside the same try as the Database open,
    // so both report as exit 4 with the "symhow query: %s" prefix.
    if flags.ensure_indices {
        if let Err(e) = crate::db::ensure_indices(&flags.db_path) {
            eprintln!("symhow query: {}", e.0);
            return 4;
        }
    }
    let db = match Database::open(flags.db_path.as_str(), flags.verbosity) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("symhow query: {}", e.0);
            return 4;
        }
    };

    // A database left with symhow_index_in_progress="1" is an interrupted
    // `symhow index` run -- still fully queryable (read-only), just
    // incomplete. One-line stderr notice, suppressed by -q/--quiet or --json.
    if !flags.json && !flags.quiet {
        let in_progress = get_meta(db.conn(), "symhow_index_in_progress");
        if in_progress.as_deref() == Some("1") {
            eprintln!("symhow query: index incomplete (interrupted) -- run `symhow update` to finish");
        }
    }

    match query_body(&db, &verb, &flags, &positional, &kind_filter, &edge_kind_filter, &location_kind_filter) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("symhow query: error: {}", e.0);
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn query_body(
    db: &Database,
    verb: &str,
    flags: &QueryFlags,
    positional: &str,
    kind_filter: &[i32],
    edge_kind_filter: &[i32],
    location_kind_filter: &[i32],
) -> Result<i32> {
    let engine = QueryEngine::new(db)?;
    let node_kind_mask = edge_kind_mask_or(kind_filter, 0);

    let json_writer = if flags.json {
        Some(JsonWriter::new(db, &engine, symhow_version()))
    } else {
        None
    };

    let render_bfs = |r: &crate::query::BfsResult| -> Result<()> {
        let ids: Vec<i64> = r.nodes.iter().map(|n| n.id).collect();
        let locs = engine.primary_locations(&ids)?;
        print!("{}", write_bfs_result(r, &locs, flags.verbosity > 0, None, false));
        Ok(())
    };

    if verb == "search" {
        let t0 = Instant::now();
        let hits = engine.search(
            positional,
            if kind_filter.is_empty() { None } else { Some(kind_filter) },
            flags.limit,
        )?;
        let elapsed_ms = millis_since(t0);
        if flags.json {
            let ctx = QueryContext {
                verb: verb.to_string(),
                db: flags.db_path.clone(),
                symbol_or_text_or_path: positional.to_string(),
                limit: flags.limit,
                search_kinds: kind_filter.to_vec(),
                ..Default::default()
            };
            println!(
                "{}",
                json_writer
                    .as_ref()
                    .unwrap()
                    .search(&ctx, &hits, elapsed_ms)?
                    .to_string()
            );
        } else {
            print!("{}", write_search_hits(&hits));
        }
        return Ok(0);
    }

    if verb == "stats" {
        let sr = engine.stats(flags.errors_limit)?;
        if flags.json {
            let ctx = QueryContext {
                verb: verb.to_string(),
                db: flags.db_path.clone(),
                ..Default::default()
            };
            println!("{}", json_writer.as_ref().unwrap().stats(&ctx, &sr)?.to_string());
        } else {
            print!("{}", write_stats(&sr));
        }
        return Ok(0);
    }

    if verb == "file" {
        let line_range = if flags.has_line_range {
            Some((flags.line_a, flags.line_b))
        } else {
            None
        };
        let loc_filter: Option<&[i32]> = if location_kind_filter.is_empty() {
            None
        } else {
            Some(location_kind_filter)
        };

        let json_ctx = if flags.json {
            QueryContext {
                verb: verb.to_string(),
                db: flags.db_path.clone(),
                symbol_or_text_or_path: positional.to_string(),
                ..Default::default()
            }
        } else {
            QueryContext::default()
        };

        let rr = resolve_file_path(db, positional)?;
        if rr.outcome == ResolveOutcome::NotFound {
            let candidates: Vec<(i64, String)> = engine
                .file_map()
                .iter()
                .map(|(id, path)| (*id, path.clone()))
                .collect();
            let hits = fuzzy_match(positional, &candidates, 5);
            if flags.json {
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .not_found_files(&json_ctx, &hits, engine.file_map())?
                        .to_string()
                );
            } else {
                eprintln!("symhow query: file '{}' not found.", positional);
                if !hits.is_empty() {
                    eprintln!("did you mean:");
                    for h in &hits {
                        match engine.file_map().get(&h.node_id) {
                            Some(path) => eprintln!("  {}", path),
                            None => eprintln!("  ?"),
                        }
                    }
                }
            }
            return Ok(3);
        }
        if rr.outcome == ResolveOutcome::Ambiguous && !flags.all_matches {
            if flags.json {
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .ambiguous(&json_ctx, &rr.candidates)?
                        .to_string()
                );
            } else {
                println!("symhow query: '{}' matches multiple files:", positional);
                let rows: Vec<CandidateRow> = rr
                    .candidates
                    .iter()
                    .map(|c| CandidateRow {
                        id: c.id,
                        kind: "FILE".to_string(),
                        name: c.serialized_name.clone(),
                        location: String::new(),
                    })
                    .collect();
                print!("{}", write_candidate_table(&rows));
                println!("Pass --all-matches to show every candidate.");
            }
            return Ok(2);
        }
        for c in &rr.candidates {
            let fr = engine.file_info(
                c.id,
                flags.want_contents,
                flags.want_occurrences,
                line_range,
                loc_filter,
            )?;
            if flags.json {
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .file(&json_ctx, &fr, flags.want_contents, flags.want_occurrences)?
                        .to_string()
                );
            } else {
                print!("{}", write_file_result(&fr));
                println!();
            }
        }
        return Ok(0);
    }

    // Every remaining verb takes a resolved symbol.
    let mut json_ctx = if flags.json {
        QueryContext {
            verb: verb.to_string(),
            db: flags.db_path.clone(),
            symbol_or_text_or_path: positional.to_string(),
            ..Default::default()
        }
    } else {
        QueryContext::default()
    };

    let (seeds, early_exit) = resolve_seeds_or_report(
        db,
        &engine,
        json_writer.as_ref(),
        &json_ctx,
        positional,
        if kind_filter.is_empty() { None } else { Some(kind_filter) },
        if flags.has_signature {
            Some(flags.signature.as_str())
        } else {
            None
        },
        flags.all_matches,
    )?;
    if let Some(code) = early_exit {
        return Ok(code);
    }
    json_ctx.symbol_ids = seeds.clone();
    json_ctx.node_kinds = kind_filter.to_vec();

    // Emits either the text tree or the symhow.graph/1 envelope for a graph
    // verb's already-computed BfsResult. `depth`/`max_nodes`/`edge_kinds`
    // are what the verb ACTUALLY applied -- overrides/overridden-by/
    // members/neighbors ignore --depth/--max-nodes entirely (fixed at
    // depth=1, unlimited nodes).
    let mut emit_graph = |r: &crate::query::BfsResult,
                      depth: i32,
                      max_nodes: i32,
                      edge_kinds: Vec<i32>,
                      elapsed_ms: i64|
     -> Result<()> {
        if flags.json {
            json_ctx.depth = depth;
            json_ctx.max_nodes = max_nodes;
            json_ctx.edge_kinds = edge_kinds;
            println!(
                "{}",
                json_writer
                    .as_ref()
                    .unwrap()
                    .graph(&json_ctx, r, elapsed_ms, None)?
                    .to_string()
            );
        } else {
            render_bfs(r)?;
        }
        Ok(())
    };

    match verb {
        "callers" => {
            let t0 = Instant::now();
            let r = engine.callers(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?;
            emit_graph(&r, flags.depth, flags.max_nodes, vec![edge_kind::CALL], millis_since(t0))?;
        }
        "callees" => {
            let t0 = Instant::now();
            let r = engine.callees(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?;
            emit_graph(&r, flags.depth, flags.max_nodes, vec![edge_kind::CALL], millis_since(t0))?;
        }
        "bases" => {
            let t0 = Instant::now();
            let r = engine.bases(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?;
            emit_graph(
                &r,
                flags.depth,
                flags.max_nodes,
                vec![edge_kind::INHERITANCE],
                millis_since(t0),
            )?;
        }
        "derived" => {
            let t0 = Instant::now();
            let r = engine.derived(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?;
            emit_graph(
                &r,
                flags.depth,
                flags.max_nodes,
                vec![edge_kind::INHERITANCE],
                millis_since(t0),
            )?;
        }
        "overrides" => {
            let t0 = Instant::now();
            let r = engine.overrides(&seeds, node_kind_mask)?;
            emit_graph(&r, 1, 0, vec![edge_kind::OVERRIDE], millis_since(t0))?;
        }
        "overridden-by" => {
            let t0 = Instant::now();
            let r = engine.overridden_by(&seeds, node_kind_mask)?;
            emit_graph(&r, 1, 0, vec![edge_kind::OVERRIDE], millis_since(t0))?;
        }
        "members" => {
            let t0 = Instant::now();
            let r = engine.members(&seeds, node_kind_mask)?;
            emit_graph(&r, 1, 0, vec![edge_kind::MEMBER], millis_since(t0))?;
        }
        "neighbors" => {
            let edge_mask = edge_kind_mask_or(edge_kind_filter, edge_kind::ALL);
            let t0 = Instant::now();
            let r = engine.neighbors(&seeds, edge_mask)?;
            let edge_kinds = if edge_kind_filter.is_empty() {
                decompose_edge_kind_mask(edge_mask)
            } else {
                edge_kind_filter.to_vec()
            };
            emit_graph(&r, 1, 0, edge_kinds, millis_since(t0))?;
        }
        "references" => {
            let t0 = Instant::now();
            let r = engine.references(&seeds)?;
            let elapsed_ms = millis_since(t0);
            if flags.json {
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .references(&json_ctx, &r, elapsed_ms)?
                        .to_string()
                );
            } else {
                print!("{}", write_references(&r));
            }
        }
        "definition" => {
            let t0 = Instant::now();
            let r = engine.definition(&seeds)?;
            let elapsed_ms = millis_since(t0);
            if flags.json {
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .definition(&json_ctx, &r, elapsed_ms)?
                        .to_string()
                );
            } else {
                print!("{}", write_definition(&r));
            }
        }
        "context" => {
            let t0 = Instant::now();

            let mut parts = Vec::new();
            parts.push(engine.callers(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?);
            parts.push(engine.callees(&seeds, flags.depth, flags.max_nodes, node_kind_mask)?);
            let mut edge_kinds = vec![edge_kind::CALL];

            parts.push(engine.overrides(&seeds, node_kind_mask)?);
            parts.push(engine.overridden_by(&seeds, node_kind_mask)?);
            edge_kinds.push(edge_kind::OVERRIDE);

            // CLASS/STRUCT seeds also get one-hop bases/derived.
            let mut any_seed_is_class_like = false;
            if !seeds.is_empty() {
                db.run_chunked_in(
                    "SELECT type FROM node WHERE id IN ({IN})",
                    &seeds,
                    &[],
                    |row| {
                        let kind: i32 = row.get(0)?;
                        if (kind & (node_kind::CLASS | node_kind::STRUCT)) != 0 {
                            any_seed_is_class_like = true;
                        }
                        Ok(())
                    },
                    500,
                )?;
            }
            if any_seed_is_class_like {
                parts.push(engine.bases(&seeds, 1, 0, node_kind_mask)?);
                parts.push(engine.derived(&seeds, 1, 0, node_kind_mask)?);
                edge_kinds.push(edge_kind::INHERITANCE);
            }

            let merged = merge_bfs_results(parts);
            let elapsed_ms = millis_since(t0);

            let mut notes_json_by_serialized_name: HashMap<String, serde_json::Value> =
                HashMap::new();
            let mut notes_text_by_node_id: HashMap<i64, NodeNoteText> = HashMap::new();
            if !flags.no_notes {
                let lookup = open_notes(db)?;
                let overlay_files = &lookup.overlay_files;

                // Build id -> serializedName for merged nodes, then look up
                // notes for those serialized names.
                for n in &merged.nodes {
                    let Some(note) = lookup.by_serialized_name.get(&n.serialized_name) else {
                        continue;
                    };

                    notes_json_by_serialized_name.insert(
                        n.serialized_name.clone(),
                        serde_json::json!({
                            "brief": note.brief,
                            "what": note.what,
                            "how": note.how,
                            "signature": note.signature_text,
                            "pre": note.pre,
                            "post": note.post,
                            "bugs": note.bugs,
                            "suggestions": note.suggestions,
                            "path": note.notes_path,
                            "line": note.heading_line,
                            "filled": note.filled,
                            "stale": note.stale,
                            "note_hash": note.note_hash,
                            "current_body_hash": note.current_body_hash,
                            "commit": note.note_commit,
                        }),
                    );

                    let text = NodeNoteText {
                        brief: note.brief.clone(),
                        filled: note.filled,
                        stale: note.stale,
                        full_markdown: if flags.notes_full {
                            let block = NoteBlock {
                                heading: note.qualified_name.clone(),
                                body_hash: note.note_hash.clone(),
                                commit: note.note_commit.clone(),
                                brief: note.brief.clone(),
                                what: TextSection {
                                    present: true,
                                    line: 0,
                                    text: note.what.clone(),
                                },
                                how: TextSection {
                                    present: true,
                                    line: 0,
                                    text: note.how.clone(),
                                },
                                signature: TextSection {
                                    present: true,
                                    line: 0,
                                    text: note.signature_text.clone(),
                                },
                                pre: ListSection {
                                    present: true,
                                    line: 0,
                                    items: note.pre.clone(),
                                },
                                post: ListSection {
                                    present: true,
                                    line: 0,
                                    items: note.post.clone(),
                                },
                                bugs: ListSection {
                                    present: true,
                                    line: 0,
                                    items: note.bugs.clone(),
                                },
                                suggestions: ListSection {
                                    present: true,
                                    line: 0,
                                    items: note.suggestions.clone(),
                                },
                                ..Default::default()
                            };
                            render_block(&block)
                        } else {
                            String::new()
                        },
                    };
                    notes_text_by_node_id.insert(n.id, text);
                }

                if !flags.json {
                    print_cache_outdated_hint(&overlay_files);
                }
            }

            if flags.json {
                json_ctx.depth = flags.depth;
                json_ctx.max_nodes = flags.max_nodes;
                json_ctx.edge_kinds = edge_kinds;
                let notes_ptr = if flags.no_notes {
                    None
                } else {
                    Some(&notes_json_by_serialized_name)
                };
                println!(
                    "{}",
                    json_writer
                        .as_ref()
                        .unwrap()
                        .graph(&json_ctx, &merged, elapsed_ms, notes_ptr)?
                        .to_string()
                );
            } else {
                let ids: Vec<i64> = merged.nodes.iter().map(|n| n.id).collect();
                let locs = engine.primary_locations(&ids)?;
                let notes_ptr = if flags.no_notes {
                    None
                } else {
                    Some(&notes_text_by_node_id)
                };
                print!("{}", write_bfs_result(
                    &merged,
                    &locs,
                    flags.verbosity > 0,
                    notes_ptr,
                    flags.notes_full,
                ));
            }
        }
        _ => {
            print_query_usage_error(&format!("unhandled verb '{}'", verb));
            return Ok(1);
        }
    }
    Ok(0)
}

/// "notes cache out of date" stderr hint (Cli.cpp's printCacheOutdatedHint),
/// shared with the `symhow notes` verbs below.
pub(crate) fn print_cache_outdated_hint(overlay_files: &[String]) {
    if !overlay_files.is_empty() {
        eprintln!(
            "symhow notes: notes cache out of date ({} file{} changed) -- run `symhow notes sync`",
            overlay_files.len(),
            if overlay_files.len() == 1 { "" } else { "s" }
        );
    }
}
