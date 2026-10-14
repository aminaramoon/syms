//! CLI-shelling tests for `symhow graph VERB ...` (Rust-only subcommand),
//! following tests/cli_dbdiff.rs's pattern: run the REAL binary via
//! `env!("CARGO_BIN_EXE_symhow")` and assert on exit codes, stdout/stderr,
//! the five `symhow.graph.*/1` JSON envelopes, and `--dot` file output.
//! Like tests/query_fixture.rs, the fixture database is located via
//! $SYMHOW_FIXTURE_DB with a `../build/fixture.srctrldb` fallback.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn fixture_db_path() -> String {
    std::env::var("SYMHOW_FIXTURE_DB").unwrap_or_else(|_| "../build/fixture.srctrldb".to_string())
}

/// Runs the real `symhow` binary and returns (exit code, stdout, stderr).
fn run_symhow(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_symhow"))
        .args(args)
        .output()
        .expect("failed to spawn symhow binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Runs `symhow graph VERB -d FIXTURE [args...]` — args[0] is the verb (the
/// subcommand's grammar puts the verb before the flags).
fn run_graph(args: &[&str]) -> (i32, String, String) {
    let db = fixture_db_path();
    let mut full = vec!["graph", args[0], "-d", db.as_str()];
    full.extend_from_slice(&args[1..]);
    run_symhow(&full)
}

fn stdout_json(args: &[&str]) -> Value {
    let (code, stdout, stderr) = run_graph(args);
    assert_eq!(code, 0, "graph {:?} failed: stderr={}", args, stderr);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON: {} -- {}", e, stdout))
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("symhow_graph_cli_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// Fixture ground truth (docs/GRAPH_SCHEMA_NOTES.md §6, re-verified): 48
// nodes / 39 edges with the unsolved hub (#141) excluded.
const NODE_COUNT: i64 = 48;
const EDGE_COUNT: i64 = 39;

// --- help --------------------------------------------------------------------------

#[test]
fn graph_help_lists_the_five_verbs_and_schema_names() {
    let (code, stdout, _stderr) = run_symhow(&["graph", "--help"]);
    assert_eq!(code, 0);
    for verb in ["centrality", "pagerank", "dominators", "slice", "path"] {
        assert!(stdout.contains(verb), "help must mention '{}'", verb);
    }
    for schema in [
        "symhow.graph.centrality/1",
        "symhow.graph.pagerank/1",
        "symhow.graph.dominators/1",
        "symhow.graph.slice/1",
        "symhow.graph.path/1",
    ] {
        assert!(stdout.contains(schema), "help must mention '{}'", schema);
    }
    // Per-verb help works too.
    let (code, stdout, _stderr) = run_graph(&["path", "--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("symhow graph path"));
}

// --- JSON envelope shapes ------------------------------------------------------------

#[test]
fn centrality_json_envelope_has_the_expected_shape_and_counts() {
    let j = stdout_json(&["centrality", "--json", "--top", "0"]);
    assert_eq!(j["schema"], "symhow.graph.centrality/1");
    assert_eq!(j["symhow_version"], "0.1.0");
    assert_eq!(j["query"]["verb"], "centrality");
    assert_eq!(j["query"]["include_unsolved"], false);
    assert_eq!(j["query"]["top"], 0);
    assert_eq!(j["truncated"], false);
    assert_eq!(j["stats"]["node_count"], NODE_COUNT);
    assert_eq!(j["stats"]["edge_count"], EDGE_COUNT);
    // Normalization denominator (n-1)(n-2) = 47*46.
    assert_eq!(j["stats"]["normalized_denominator"], 47 * 46);

    let nodes = j["nodes"].as_array().unwrap();
    assert_eq!(nodes.len() as i64, NODE_COUNT);
    // Canonical node object fields plus the score, ranked by score desc.
    let first = &nodes[0];
    for key in ["id", "kind", "name", "qualified_name", "signature", "score"] {
        assert!(first.get(key).is_some(), "node object lacks '{}'", key);
    }
    assert_eq!(first["name"], "level2");
    assert_eq!(first["kind"], "FUNCTION");
    let mut previous = f64::INFINITY;
    for node in nodes {
        let score = node["score"].as_f64().unwrap();
        assert!(score <= previous, "scores must be ranked descending");
        previous = score;
    }
}

#[test]
fn centrality_include_unsolved_reincludes_the_hub() {
    let j = stdout_json(&["centrality", "--json", "--top", "0", "--include-unsolved"]);
    assert_eq!(j["query"]["include_unsolved"], true);
    assert_eq!(j["stats"]["node_count"], NODE_COUNT + 1);
    assert_eq!(j["stats"]["edge_count"], EDGE_COUNT + 3);
    // The hub is a participant now (id 141).
    let ids: Vec<i64> = j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&141));
}

#[test]
fn centrality_top_caps_both_json_and_text_output() {
    let j = stdout_json(&["centrality", "--json", "--top", "3"]);
    let nodes = j["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    assert_eq!(j["stats"]["shown"], 3);
    assert_eq!(j["truncated"], true);

    let (_code, stdout, _stderr) = run_graph(&["centrality", "--top", "3"]);
    assert_eq!(_code, 0);
    assert!(
        stdout.contains("(showing top 3 of 48 nodes; --top 0 shows all)"),
        "stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("#149"),
        "top scorer #149 expected: {}",
        stdout
    );
    assert!(!stdout.contains("#141"),);
}

#[test]
fn pagerank_json_echoes_seeds_and_scores_sum_to_one() {
    let j = stdout_json(&[
        "pagerank", "--json", "--top", "0", "--seed", "#206", "--seed", "#211",
    ]);
    assert_eq!(j["schema"], "symhow.graph.pagerank/1");
    assert_eq!(j["query"]["seeds"], serde_json::json!(["#206", "#211"]));
    assert_eq!(j["query"]["seeds_explicit"], true);
    assert_eq!(j["query"]["seed_ids"], serde_json::json!([206, 211]));
    assert_eq!(j["query"]["damping"], 0.85);
    assert_eq!(j["stats"]["seed_count"], 2);
    assert_eq!(j["stats"]["node_count"], NODE_COUNT);
    assert_eq!(j["truncated"], false);

    let nodes = j["nodes"].as_array().unwrap();
    assert_eq!(nodes.len() as i64, NODE_COUNT);
    let total: f64 = nodes.iter().map(|n| n["score"].as_f64().unwrap()).sum();
    assert!((total - 1.0).abs() < 1e-6, "rank mass sums to {}", total);
    // The two TU-local mains tie at the top.
    assert_eq!(nodes[0]["id"], 206);
    assert_eq!(nodes[1]["id"], 211);
    assert!(
        (nodes[0]["score"].as_f64().unwrap() - nodes[1]["score"].as_f64().unwrap()).abs() < 1e-9
    );
    assert!((nodes[0]["score"].as_f64().unwrap() - 0.270270).abs() < 1e-4);
}

#[test]
fn pagerank_without_seeds_falls_back_to_the_main_heuristic() {
    let j = stdout_json(&["pagerank", "--json", "--top", "4"]);
    assert_eq!(j["query"]["seeds"], serde_json::json!([]));
    assert_eq!(j["query"]["seeds_explicit"], false);
    assert_eq!(j["query"]["seed_ids"], serde_json::json!([206, 211]));
    assert_eq!(j["stats"]["seed_count"], 2);
}

#[test]
fn pagerank_unknown_seed_is_a_clean_not_found_error() {
    // Text mode: exit 3 + "symbol not found" on stderr.
    let (code, _stdout, stderr) = run_graph(&["pagerank", "--seed", "no_such_symbol_xyz"]);
    assert_eq!(code, 3);
    assert!(stderr.contains("symbol not found"), "stderr: {}", stderr);

    // JSON mode: the symhow.not-found/1 envelope on stdout, exit 3.
    let (code, stdout, _stderr) =
        run_graph(&["pagerank", "--seed", "no_such_symbol_xyz", "--json"]);
    assert_eq!(code, 3);
    let j: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(j["schema"], "symhow.not-found/1");
    assert_eq!(j["query"]["symbol"], "no_such_symbol_xyz");
}

#[test]
fn pagerank_damping_out_of_range_is_a_clean_error() {
    let (code, _stdout, stderr) = run_graph(&["pagerank", "--damping", "1.5", "--seed", "#206"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("outside [0, 1]"), "stderr: {}", stderr);
}

#[test]
fn dominators_without_entry_lists_candidates_and_exits_2() {
    let (code, _stdout, stderr) = run_graph(&["dominators"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("2 entry-point candidates"),
        "stderr: {}",
        stderr
    );
    assert!(stderr.contains("#206"), "stderr: {}", stderr);
    assert!(stderr.contains("#211"), "stderr: {}", stderr);
}

#[test]
fn dominators_json_from_runchain_has_the_eight_expected_idoms() {
    let j = stdout_json(&["dominators", "--json", "--entry", "#155"]);
    assert_eq!(j["schema"], "symhow.graph.dominators/1");
    assert_eq!(j["query"]["entry"], "#155");
    assert_eq!(j["query"]["entry_id"], 155);
    assert_eq!(j["query"]["entry_explicit"], true);
    assert_eq!(j["root_id"], 155);
    assert_eq!(j["truncated"], false);
    assert_eq!(j["stats"]["dominated_count"], 8);

    let idoms = j["idoms"].as_array().unwrap();
    assert_eq!(idoms.len(), 8);
    let got: std::collections::BTreeSet<(i64, i64)> = idoms
        .iter()
        .map(|p| (p["node"].as_i64().unwrap(), p["idom"].as_i64().unwrap()))
        .collect();
    let expected: std::collections::BTreeSet<(i64, i64)> = [
        (151, 155), // level0 <- runChain
        (152, 155), // factorial <- runChain
        (154, 155), // ping <- runChain
        (150, 151), // level1 <- level0
        (149, 150), // level2 <- level1
        (148, 149), // level3 <- level2
        (147, 148), // level4 <- level3
        (153, 154), // pong <- ping
    ]
    .into_iter()
    .collect();
    assert_eq!(got, expected);

    // nodes[] is the root plus every dominated node, sorted by id.
    let ids: Vec<i64> = j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    let expected_ids: Vec<i64> = [147, 148, 149, 150, 151, 152, 153, 154, 155].to_vec();
    assert_eq!(ids, expected_ids);
}

#[test]
fn dominators_text_mode_prints_an_indented_tree() {
    let (code, stdout, _stderr) = run_graph(&["dominators", "--entry", "#155"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("dominator tree from runChain() (#155) -- 9 of 48 nodes dominated"),
        "stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("  #147"),
        "indented level4 expected: {}",
        stdout
    );
}

#[test]
fn dominators_dot_writes_idom_tree_edges() {
    let dir = fresh_dir("dom_dot");
    let dot_path = dir.join("dominators.dot");
    let (code, _stdout, stderr) = run_graph(&[
        "dominators",
        "--entry",
        "#155",
        "--dot",
        &dot_path.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    let dot = std::fs::read_to_string(&dot_path).unwrap();
    assert!(dot.contains("digraph dominators"));
    for edge in [
        "155 -> 151;",
        "155 -> 152;",
        "155 -> 154;",
        "150 -> 149;",
        "148 -> 147;",
    ] {
        assert!(dot.contains(edge), "dot must contain '{}':\n{}", edge, dot);
    }
    assert!(
        dot.contains("label=\"level4() (FUNCTION)\""),
        "dot: {}",
        dot
    );
}

#[test]
fn slice_json_of_global_counter_is_the_pair_with_its_site() {
    let j = stdout_json(&["slice", "--json", "--criterion", "gGlobalCounter"]);
    assert_eq!(j["schema"], "symhow.graph.slice/1");
    assert_eq!(j["query"]["criterion"], "gGlobalCounter");
    assert_eq!(j["query"]["criterion_id"], 59);
    assert_eq!(j["truncated"], false);
    assert_eq!(j["stats"]["member_count"], 2);
    assert_eq!(j["stats"]["edge_count"], 1);

    let ids: Vec<i64> = j["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![59, 60]);

    let edges = j["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["source"], 60);
    assert_eq!(edges[0]["target"], 59);
    assert_eq!(edges[0]["kind"], "USAGE");
    assert_eq!(edges[0]["call_sites"], 1);
    let locations = edges[0]["locations"].as_array().unwrap();
    assert_eq!(locations.len(), 1);
    assert!(locations[0]["file"].as_str().unwrap().ends_with("ns.cpp"));
    assert_eq!(locations[0]["start_line"], 65);
}

#[test]
fn slice_text_with_lines_annotates_reference_sites() {
    // Without --lines: no site annotations.
    let (code, stdout, _stderr) = run_graph(&["slice", "--criterion", "#27"]);
    assert_eq!(code, 0);
    assert!(
        stdout
            .contains("backward slice of fixture::Square::area() const (#27) -- 3 nodes, 4 edges"),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains("#33"), "stdout: {}", stdout);
    assert!(
        !stdout.contains(" @ "),
        "--lines off must not annotate: {}",
        stdout
    );

    // With --lines: each edge carries its ns.cpp site.
    let (code, stdout, _stderr) = run_graph(&["slice", "--criterion", "#27", "--lines"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("@ "), "stdout: {}", stdout);
    assert!(stdout.contains("ns.cpp:55"), "stdout: {}", stdout);
    assert!(stdout.contains("ns.cpp:66"), "stdout: {}", stdout);
}

#[test]
fn slice_dot_contains_the_induced_edges() {
    let dir = fresh_dir("slice_dot");
    let dot_path = dir.join("slice.dot");
    let (code, _stdout, stderr) = run_graph(&[
        "slice",
        "--criterion",
        "#27",
        "--dot",
        &dot_path.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    let dot = std::fs::read_to_string(&dot_path).unwrap();
    assert!(dot.contains("digraph slice"));
    // Node names are DB ids; both members of the parallel CALL+USAGE pair.
    assert!(dot.contains("60 -> 33"), "dot: {}", dot);
    assert!(dot.contains("33 -> 27"), "dot: {}", dot);
    assert!(dot.contains("[label=\"USAGE\"]"), "dot: {}", dot);
    assert!(
        dot.contains("[label=\"fixture::Square::area() const (METHOD)\"]"),
        "dot: {}",
        dot
    );
}

#[test]
fn slice_missing_or_foreign_criterion_errors_cleanly() {
    // Missing --criterion: usage error.
    let (code, _stdout, stderr) = run_graph(&["slice"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("missing required option --criterion"),
        "stderr: {}",
        stderr
    );

    // Unknown symbol: not found (exit 3).
    let (code, _stdout, stderr) = run_graph(&["slice", "--criterion", "no_such_symbol_xyz"]);
    assert_eq!(code, 3);
    assert!(stderr.contains("symbol not found"), "stderr: {}", stderr);

    // The unsolved hub is excluded from the graph (exit 3 + hint).
    let (code, _stdout, stderr) = run_graph(&["slice", "--criterion", "#141"]);
    assert_eq!(code, 3);
    assert!(
        stderr.contains("not a participant") && stderr.contains("unsolved"),
        "stderr: {}",
        stderr
    );
}

#[test]
fn path_json_reports_all_three_weight_schemes() {
    for (weight, cost) in [("hops", 5.0), ("calls", 5.0), ("lines", 20.0)] {
        let j = stdout_json(&[
            "path", "--json", "--from", "#155", "--to", "#147", "--weight", weight,
        ]);
        assert_eq!(j["schema"], "symhow.graph.path/1");
        assert_eq!(j["query"]["from_id"], 155);
        assert_eq!(j["query"]["to_id"], 147);
        assert_eq!(j["query"]["weight"], weight);
        assert_eq!(j["found"], true);
        assert_eq!(j["truncated"], false);
        assert_eq!(j["stats"]["total_cost"], cost, "weight {}", weight);
        assert_eq!(j["stats"]["hop_count"], 5);
        let ids: Vec<i64> = j["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![155, 151, 150, 149, 148, 147], "weight {}", weight);
        let edges = j["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 5);
        for edge in edges {
            assert_eq!(edge["kind"], "CALL");
        }
    }
}

#[test]
fn path_unreachable_target_is_a_result_not_an_error() {
    // runChain's component never reaches the TU-local helper.
    let (code, stdout, _stderr) = run_graph(&["path", "--from", "#155", "--to", "#205"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("no path from"), "stdout: {}", stdout);

    let (code, stdout, _stderr) = run_graph(&["path", "--json", "--from", "#155", "--to", "#205"]);
    assert_eq!(code, 0);
    let j: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(j["schema"], "symhow.graph.path/1");
    assert_eq!(j["found"], false);
    assert_eq!(j["stats"]["total_cost"], Value::Null);
    assert_eq!(j["nodes"].as_array().unwrap().len(), 0);
    assert_eq!(j["edges"].as_array().unwrap().len(), 0);
}

#[test]
fn path_dot_writes_the_walked_chain_or_an_empty_digraph() {
    let dir = fresh_dir("path_dot");

    let dot_path = dir.join("chain.dot");
    let (code, _stdout, stderr) = run_graph(&[
        "path",
        "--from",
        "#155",
        "--to",
        "#147",
        "--dot",
        &dot_path.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    let dot = std::fs::read_to_string(&dot_path).unwrap();
    assert!(dot.contains("digraph path"));
    assert!(
        dot.contains("label=\"runChain() (FUNCTION)\""),
        "dot: {}",
        dot
    );
    for edge in [
        "155 -> 151 [label=\"CALL\"];",
        "151 -> 150 [label=\"CALL\"];",
        "149 -> 148 [label=\"CALL\"];",
        "148 -> 147 [label=\"CALL\"];",
    ] {
        assert!(dot.contains(edge), "dot must contain '{}':\n{}", edge, dot);
    }

    // Unreachable: a valid, empty digraph.
    let dot_path = dir.join("unreachable.dot");
    let (code, _stdout, stderr) = run_graph(&[
        "path",
        "--from",
        "#155",
        "--to",
        "#205",
        "--dot",
        &dot_path.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    let dot = std::fs::read_to_string(&dot_path).unwrap();
    assert_eq!(dot, "digraph path {\n}\n");
}

#[test]
fn path_usage_errors_are_clean() {
    // Missing --to.
    let (code, _stdout, stderr) = run_graph(&["path", "--from", "#155"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("missing required option --to"),
        "stderr: {}",
        stderr
    );

    // Unknown --weight value.
    let (code, _stdout, stderr) = run_graph(&[
        "path", "--from", "#155", "--to", "#147", "--weight", "bogus",
    ]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("unknown --weight value 'bogus'"),
        "stderr: {}",
        stderr
    );

    // Unknown symbol for --from: not found (exit 3).
    let (code, _stdout, stderr) =
        run_graph(&["path", "--from", "no_such_symbol_xyz", "--to", "#147"]);
    assert_eq!(code, 3);
    assert!(stderr.contains("symbol not found"), "stderr: {}", stderr);
}

// --- shared error handling -------------------------------------------------------------

#[test]
fn missing_database_exits_4_with_a_clean_stderr_message() {
    let (code, _stdout, stderr) =
        run_symhow(&["graph", "centrality", "-d", "/nonexistent/path/x.srctrldb"]);
    assert_eq!(code, 4);
    assert!(
        stderr.contains("symhow graph centrality: failed to open"),
        "stderr: {}",
        stderr
    );
}

#[test]
fn unknown_verb_and_missing_verb_are_usage_errors() {
    let (code, _stdout, stderr) = run_symhow(&["graph", "bogusverb"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("unknown verb 'bogusverb'"),
        "stderr: {}",
        stderr
    );

    let (code, _stdout, stderr) = run_symhow(&["graph"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("missing VERB"), "stderr: {}", stderr);
}

#[test]
fn unknown_options_and_bad_values_are_usage_errors() {
    let (code, _stdout, stderr) = run_graph(&["centrality", "--bogus"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("unknown option '--bogus'"),
        "stderr: {}",
        stderr
    );

    let (code, _stdout, stderr) = run_graph(&["centrality", "--top", "abc"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("'--top' expects a non-negative integer"),
        "stderr: {}",
        stderr
    );

    // Flags that need a value.
    let (code, _stdout, stderr) = run_graph(&["slice", "--criterion"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("requires a value"), "stderr: {}", stderr);
}

#[test]
fn dot_flag_creates_missing_parent_directories() {
    let dir = fresh_dir("dot_mkdir");
    let dot_path = dir.join("a/b/deep.dot");
    let (code, _stdout, stderr) = run_graph(&[
        "centrality",
        "--top",
        "2",
        "--dot",
        &dot_path.to_string_lossy(),
    ]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    let dot = std::fs::read_to_string(&dot_path).unwrap();
    assert!(dot.starts_with("digraph"), "dot: {}", dot);
    // The induced-subgraph DOT uses petgraph's `label = "..."` spacing.
    assert!(dot.contains("\"level2() (FUNCTION)\""), "dot: {}", dot);
    assert!(dot.contains("\"level1() (FUNCTION)\""), "dot: {}", dot);
}
