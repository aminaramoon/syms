# Graph-algorithm inputs from the `.srctrldb` schema

Task-1 deliverable: what the Sourcetrail-schema SQLite database written by
`symhow index` can (and cannot) supply for the five planned graph analyses.
Everything below was verified against the real databases
`build/fixture.srctrldb` (C++ synthetic corpus, 83 nodes / 106 edges) and
`build/cuda_fixture.srctrldb` (CUDA corpus, 27 nodes / 29 edges), and
cross-checked against `rust/src/db/{database,enums,name_decode,meta}.rs`,
`rust/src/query/{bfs,query_engine,results}.rs`, and the C++ indexer
(`src/index/AstWalker.cpp`, `src/model/Facts.h`).

**Audience**: the petgraph-integration teammate. All SQL is copy-pasteable
against either fixture DB.

---

## 1. Schema overview

`meta.storage_version = "25"` (string). `rust/src/db/database.rs` hard-fails
on anything else — graph verbs should reuse `Database::open()` and inherit
that check.

### 1.1 Core Sourcetrail tables

| Table | Columns | Row meaning |
|---|---|---|
| `meta` | `id, key, value` | `storage_version` ("25"), `project_settings` (XML), `symhow_*` provenance keys (see §1.3) |
| `element` | `id` | Parent id-space: every `node.id`, `edge.id`, `local_symbol.id`, `error.id` is also an `element.id`. In fixture: 227 = 83 nodes + 106 edges + 35 local_symbols + 3 errors |
| `node` | `id, type, serialized_name` | One graph vertex per C++ symbol/file. `type` is a **single-bit NodeKind mask** (§2.1). `serialized_name` is the encoded name hierarchy (§3) |
| `edge` | `id, type, source_node_id, target_node_id` | One directed edge. `type` is a single-bit EdgeKind (§2.2). **Unique on (source, target, type)** — verified: 0 duplicate triples in fixture. So parallel edges do NOT exist; multiplicity must be recovered from `occurrence` (§4.5) |
| `symbol` | `id, definition_kind` | Extension row for indexable symbols (files/macros/unsolved have none). `definition_kind`: 1=IMPLICIT (never produced), 2=EXPLICIT. A `node` with no `symbol` row = non-symbol node (file/macro/unsolved) |
| `file` | `id, path, language, modification_time, indexed, complete, line_count` | File node. `file.id` is the SAME id as the file's `node.id` (no separate "file symbol" node — `KNOWN_LIMITATIONS.md`) |
| `filecontent` | `id, content` | Full source text of each file (fixture: ~270–1250 bytes/file). Column type is literally misspelled `INTERGER` in the DDL — harmless, SQLite accepts |
| `local_symbol` | `id, name` | Function-local symbols (params, locals, template params). **`name` is a location string** `"path:line:col"` of the declaration site — so the def site is recoverable from the row itself |
| `source_location` | `id, file_node_id, start_line, start_column, end_line, end_column, type` | A span in a file. `type` is a LocationKind (§2.3) |
| `occurrence` | `element_id, source_location_id` (PK pair) | Ties an element (node, edge, **or local_symbol**) to a span. This is the only way to map edges/locals back to code lines |
| `error` | `id, message, fatal, indexed, translation_unit` | Indexer diagnostics (non-fatal in fixture) |
| `component_access`, `element_component` | — | **Always empty** in symhow DBs (Sourcetrail name-hierarchy components unused). Ignore them |

### 1.2 symhow side tables (present in every symhow-written DB)

| Table | Meaning |
|---|---|
| `symhow_file_fingerprint(file_node_id, sha256)` | Content hash per file |
| `symhow_symbol_def(node_id, file_node_id)` WITHOUT ROWID | Files in which a symbol is declared/defined (one row per pair) |
| `symhow_tu(path, args_hash, file_node_id, status, entry_count)` | One row per compile_commands.json TU; `file_node_id` = TU's primary file node |
| `symhow_touched(node_id)` | Node ids produced by the most recent index/update pass (fixture: all 83) |

### 1.3 symhow meta keys (provenance)

`symhow_provenance_version`, `symhow_version`, `symhow_walker_version`,
`symhow_cdb_path`, `symhow_options_json` (index flags: c_std, cuda_mode,
include dirs, …), `symhow_repo_root`, `symhow_repo_commit`,
`symhow_dirty_paths`, `symhow_indexed_at`, `symhow_last_update_at`.

> ⚠️ `symhow_dirty_paths` is a ~2 MB JSON array of absolute paths. **Never
> `SELECT * FROM meta`** in a CLI verb — select specific keys.

### 1.4 Indices available (created by `symhow index`, see `database.rs`)

`edge(source_node_id)`, `edge(target_node_id)`, `edge(type)`,
`node(type)`, `node(serialized_name)`, `source_location(file_node_id)`,
`occurrence(element_id)`, `occurrence(source_location_id)`, `file(path)`,
`symbol(definition_kind)` — adjacency both directions is already indexed;
bulk edge scans will be fast.

---

## 2. Enum tables (integer codes actually on disk)

From `rust/src/db/enums.rs` (mirrors `third_party/sourcetraildb/include/*.h`).
All kinds are **single bits**; symhow filters with `(col & mask) != 0`
(see `bfs.rs:120`), so reuse that idiom.

### 2.1 `node.type` (NodeKind)

| Value | Name | fixture count | cuda count |
|---:|---|---:|---:|
| 1<<0 = 1 | UNKNOWN | 1 | 0 |
| 1<<1 = 2 | TYPE | 3 | 0 |
| 1<<4 = 16 | NAMESPACE | 5 | 0 |
| 1<<6 = 64 | STRUCT | 3 | 3 |
| 1<<7 = 128 | CLASS | 6 | 0 |
| 1<<10 = 1024 | GLOBAL_VARIABLE | 1 | 0 |
| 1<<11 = 2048 | FIELD | 7 | 5 |
| 1<<12 = 4096 | FUNCTION | 26 | 5 |
| 1<<13 = 8192 | METHOD | 14 | 3 |
| 1<<14 = 16384 | ENUM | 1 | 0 |
| 1<<15 = 32768 | ENUM_CONSTANT | 3 | 0 |
| 1<<16 = 65536 | TYPEDEF | 2 | 3 |
| 1<<18 = 262144 | FILE | 9 | 2 |
| 1<<19 = 524288 | MACRO | 2 | 6 |

Not observed anywhere (still decode-able): BUILTIN_TYPE(4), MODULE(8),
PACKAGE(32), INTERFACE(256), ANNOTATION(512), TYPE_PARAMETER(131072),
UNION(1<<20).

> `node.type = 1` (UNKNOWN) is **always the single shared "unsolved symbol"
> node** created by the vendored writer for every unresolvable reference
> (`KNOWN_LIMITATIONS.md` §Phase 4). It is one hub node that receives CALL
> and USAGE edges from every function with an unresolved call/access — in
> fixture: 3 CALL edges point at it. Graph algorithms should either exclude
> it or report it specially, or it will dominate centrality/PR results.
> `type=1` rows other than this one would be bugs.

### 2.2 `edge.type` (EdgeKind)

| Value | Name | Direction (source → target), verified in fixture | fixture | cuda |
|---:|---|---|---:|---:|
| 0 | UNKNOWN | — | 0 | 0 |
| 1<<0 = 1 | MEMBER | container → member (namespace→class/namespace, class/struct→method/field, file→macro) | 35 | 8 |
| 1<<1 = 2 | TYPE_USAGE | user (function/method/class) → type node | 13 | 9 |
| 1<<2 = 4 | USAGE | using function/method → used symbol (field read/write, global var, function ref, enum constant) | 11 | 2 |
| 1<<3 = 8 | CALL | caller (function/method) → callee (function/method, or unsolved node) | 31 | 5 |
| 1<<4 = 16 | INHERITANCE | derived → base | 4 | 0 |
| 1<<5 = 32 | OVERRIDE | derived method → base method | 5 | 0 |
| 1<<6 = 64 | TYPE_ARGUMENT | — | 0 | 0 |
| 1<<7 = 128 | TEMPLATE_SPECIALIZATION | — | 0 | 0 |
| 1<<8 = 256 | INCLUDE | includer file → included file | 4 | 0 |
| 1<<9 = 512 | IMPORT | — | 0 | 0 |
| 1<<11 = 2048 | MACRO_USAGE | file → macro (context is always the *file* node) | 3 | 5 |
| 1<<12 = 4096 | ANNOTATION_USAGE | — | 0 | 0 |

### 2.3 `source_location.type` (LocationKind)

TOKEN=0 (most common), SCOPE=1 (full definition body span), QUALIFIER=2,
LOCAL_SYMBOL=3 (local-symbol spans), SIGNATURE=4, ATOMIC_RANGE=5,
INDEXER_ERROR=6, FULLTEXT_SEARCH=7, SCREEN_SEARCH=8, UNSOLVED=9.

The existing query engine treats `l.type IN (0,1,3,9)` as reference-worthy
(`query_engine.rs:270`) and `type = 0` as the primary location
(`primary_locations`), preferring SCOPE over TOKEN for definitions
(`definition`, `query_engine.rs:341`).

### 2.4 Occurrence ↔ edge relationship (important)

- Every **non-MEMBER** edge has ≥1 `occurrence` row: `occurrence.element_id =
  edge.id`, and the joined `source_location` is the *source-code site of the
  reference* (e.g. the line of the call). **MEMBER edges have zero
  occurrences** (structural, verified: all 35 fixture MEMBER edges).
- Consequence: per-edge multiplicity IS recoverable — `COUNT` of occurrences
  of a CALL edge = number of call sites in the caller (§4.5, §6).

---

## 3. `serialized_name` decoding (needed for every node label)

Format (inverse implemented in `rust/src/db/name_decode.rs`; C++ twin
`symhow::db::NameDecode`):

```
<name_delimiter> \tm <elem> \tn <elem> \tn ...      where elem =
  <name> \ts <prefix> \tp <postfix>
```

- `name_delimiter` is `"::"` for symbols, `"/"` for **file** nodes (elements
  are path components — display name = last component).
- `prefix`/`postfix` carry the signature: e.g. function
  `name="squarePerimeter", prefix="double ", postfix="(const Square &)"`.
- TU-local symbols carry a disambiguator postfix ` [tu_a.cpp#04c3b899]`
  (` [file#8hex]`). `display_name()` strips it; `qualified_name()` keeps it.

**Cross-TU consequence for graphs**: statics/TU-local functions in different
TUs are *distinct* nodes (fixture has two `helper` + two `main` nodes,
one per TU). The call graph does not merge them; entry-point detection by
name `main` can legitimately return several nodes.

Sample rows (fixture):

```
id=33 FUNCTION ::	mfixture	s	p	nsquarePerimeter	sdouble 	p(const Square &)
id=27 METHOD  ::	mfixture	s	p	nSquare	s	p	narea	sdouble 	p() const
id=1  UNKNOWN 	munsolved symbol	s	p            <- the shared unsolved node
id=2  FILE     /	m/Users/.../ns.cpp	s	p
```

---

## 4. Reusable extraction SQL

### 4.1 Nodes (call-graph participants)

```sql
-- functions + methods only (drop the unsolved node explicitly if desired)
SELECT n.id, n.type, n.serialized_name
FROM   node n
WHERE  (n.type & 4096) != 0      -- FUNCTION
   OR  (n.type & 8192) != 0      -- METHOD
```

### 4.2 Call graph, with call-site multiplicity

```sql
SELECT e.id,
       e.source_node_id AS caller,
       e.target_node_id AS callee,
       COUNT(o.source_location_id) AS call_sites
FROM   edge e
LEFT   JOIN occurrence o ON o.element_id = e.id
WHERE  e.type = 8                       -- CALL
GROUP  BY e.id;
```

Join `node` twice for names, and filter
`AND e.target_node_id != (SELECT id FROM node WHERE type = 1)` to drop the
unsolved hub.

### 4.3 Include graph (file-level dependency)

```sql
SELECT e.source_node_id, e.target_node_id FROM edge e WHERE e.type = 256;  -- INCLUDE
```

### 4.4 Def/use edges (PDG approximation)

```sql
SELECT e.id, e.source_node_id AS context_fn, e.target_node_id AS accessed,
       e.type,                                   -- 4=USAGE, 8=CALL
       l.file_node_id, l.start_line, l.end_line  -- ref site (via occurrence)
FROM   edge e
JOIN   occurrence o ON o.element_id = e.id
JOIN   source_location l ON l.id = o.source_location_id
WHERE  e.type IN (4, 8);
```

### 4.5 Local-variable def/use sites (no edges — locations only)

```sql
-- def site is encoded in local_symbol.name ("path:line:col");
-- other occurrences of the same id are uses.
SELECT ls.id, ls.name AS def_site,
       l.file_node_id, l.start_line, l.start_column
FROM   local_symbol ls
JOIN   occurrence o ON o.element_id = ls.id
JOIN   source_location l ON l.id = o.source_location_id
WHERE  l.type = 3          -- LOCAL_SYMBOL
ORDER  BY ls.id, l.start_line, l.start_column;
```

### 4.6 Synthetic per-function "size" (for weights)

```sql
-- body span of every defined function/method (SCOPE location)
SELECT o.element_id AS fn_node_id,
       l.end_line - l.start_line + 1 AS body_lines
FROM   occurrence o
JOIN   source_location l ON l.id = o.source_location_id
WHERE  l.type = 1          -- SCOPE
  AND  o.element_id IN (SELECT id FROM node WHERE type IN (4096, 8192));
```

(36 such spans in fixture.)

---

## 5. Per-algorithm analysis

Common petgraph recommendation for all five:
`petgraph::graph::DiGraph<i64, E>` with **node weight = the DB node id**
(`i64`). Hydrate names/kinds lazily via the existing chunked-IN helper
(`Database::run_chunked_in`, chunk 500 — same pattern as `bfs.rs`). Keep the
`HashMap<i64, NodeIndex>` side-map built during load; graph verbs must return
DB ids to stay compatible with `symhow.graph/1` JSON output
(`results.rs` / `JsonWriter` emit paths). `petgraph` is **not yet in
`Cargo.toml`** — it must be added (no other deps needed; no `serde`
feature required).

| # | Algorithm | Graph | Verdict |
|---|---|---|---|
| 1 | Dominator tree | call graph | **approximate** (function granularity) |
| 2 | Betweenness centrality | call graph (or include graph) | **fully supported** (manual Brandes) |
| 3 | Personalized PageRank | call graph | **fully supported** (manual personalized PR) |
| 4 | Backward program slicing | USAGE+CALL reverse closure | **approximate** (function granularity, no read/write split) |
| 5 | Weighted shortest path | weighted call graph | **fully supported** structurally; weights **synthetic** |

### 5.1 Dominator tree

- **Supply**: §4.2 call graph. Nothing finer exists — **the schema has no
  basic blocks, no statement, and no intra-procedural control flow
  whatsoever** (`AstWalker.cpp` emits only symbol-level references).
- **Approximation**: dominators of the *call graph* rooted at an entry point
  — "which functions must have been called on every path from entry to X".
  Semantically meaningful and cheap, but not a true CFG dominator tree.
- **Missing**: basic-block CFG, branch/loop structure, intra-function edges.
- **petgraph**: `DiGraph<i64, ()>` +
  `petgraph::algo::dominators::simple_fast(&g, root)` (exists in 0.6→0.8).
  Needs a single root; pick an entry-point node (§5.3 heuristics). Handles
  the recursion cycles the fixture contains (`factorial` self-loop,
  `ping⇄pong` cycle).
- **SQL**: §4.2, minus the unsolved node.
- **Verdict: approximate** — valid, but document that it is call-graph
  dominance, not control-flow dominance.

### 5.2 Betweenness centrality

- **Supply**: same call graph (§4.2); the INCLUDE graph (§4.3) is a good
  second target (file-level coupling).
- **Missing**: nothing structural. One caveat: the shared unsolved node is a
  high-traffic hub that would distort scores — exclude or keep behind a flag.
- **petgraph**: **no built-in betweenness** in petgraph ≤0.8
  (checked the `petgraph::algo` module index on docs.rs, 0.8.3: dijkstra,
  astar, floyd_warshall, k_shortest_path, page_rank, scc, dominators, … —
  no betweenness). Implement Brandes' algorithm manually (~50 lines,
  unweighted BFS accumulation) or pull a dedicated crate; recommend manual
  to avoid a new dependency.
- **petgraph**: `DiGraph<i64, ()>`; run per connected component or on the
  whole graph (isolated nodes just get score 0).
- **Verdict: fully supported** (at symbol granularity, minus unsolved hub).

### 5.3 Personalized PageRank

- **Supply**: call graph (§4.2). Edge weight = `call_sites` (occurrence
  count) or uniform 1.
- **Seeds ("entry points")**: no entry-point metadata exists in the schema.
  Heuristics, in order of quality:
  1. explicit `--seed` symbol args (best; matches existing `callers/callees`
     seed resolution via `symbol_resolver.rs`),
  2. FUNCTION nodes whose decoded name is `main` (may be several — see §3),
  3. file nodes with zero incoming INCLUDE edges, then their FUNCTION/METHOD
     descendants via MEMBER edges.
- **Missing**: profile/frequency data (§5.5 note applies).
- **petgraph**: `petgraph::algo::page_rank` exists but is **not
  personalized** (standard PR over structure with uniform teleport). Implement
  the personalized variant as a ~30-line power iteration over
  `DiGraph<i64, f64>` (or `f32`) using a `HashMap<NodeIndex, f64>`
  personalization vector; don't fight the built-in's signature.
- **Verdict: fully supported** (synthetic weights, heuristic seeds).

### 5.4 Backward program slicing

- **Supply**: symbol-level dependence edges: USAGE (§4.4) is a
  context-function → accessed-symbol edge regardless of read or write;
  CALL contributes parameter/data flow at function granularity. The
  occurrence join gives the *source line* of each dependence, which lets the
  slice be presented as line ranges inside functions.
- **Def-use of locals**: local symbols have **no edges at all**
  (`KNOWN_LIMITATIONS.md`: "Local symbols never become edge endpoints").
  Only def-site (`local_symbol.name = "path:line:col"`) and use-site
  (`occurrence` rows, `l.type = 3`) locations — §4.5. A "slice" for a local
  variable can therefore only be "all lines mentioning it in its function",
  extracted from `filecontent`, not a data-flow slice.
- **Approximation that works**: backward slice of a *global/field/heap
  symbol* S = reverse-transitive closure of {S} over USAGE edges (read/write
  ambiguity: an assignment and a read both produce one USAGE edge), expanded
  through CALL edges for interprocedural flow. Report the closure plus the
  per-edge source lines (SQL §4.4) — i.e. *function-granularity backward
  slice*, not statement-level.
- **Missing**: statement-level PDG, read/write (def vs use) distinction,
  aliasing/pointer analysis, intra-procedural dependences, control
  dependence.
- **petgraph**: `DiGraph<i64, EdgeKind>` (or `(kind, line)` tuple) and a
  manual reverse DFS/BFS closure (petgraph's `Reversed` view +
  `Dfs`/`Bfs` from `petgraph::visit`).
- **Verdict: approximate** — usable coarse slicing; statement-level slicing
  is not possible from this schema.

### 5.5 Weighted shortest path

- **Supply**: call graph. **No profile, timing, or counter data exists
  anywhere in the schema** (checked every table + `symhow_options_json`) —
  weights must be synthetic. Options, all derivable from the DB:
  1. uniform 1 (hop count),
  2. call-site count from §4.2 (`call_sites` as edge weight — "how many
     places call this"),
  3. callee body size from §4.6 (`body_lines` as traversal cost — "how much
     code you descend into"),
  4. CLI-supplied `--weight` overrides per symbol.
- **Missing**: real execution frequencies; multiplicity as separate parallel
  edges (schema dedups — use occurrence count instead).
- **petgraph**: `DiGraph<i64, u64>` (options 1–2) or `DiGraph<i64, f64>`
  (option 3); `petgraph::algo::dijkstra` (both u64 and f64 implement
  `Measure`) or `astar` for single-target. Negative weights impossible → no
  Bellman-Ford needed.
- **Verdict: fully supported** structurally; weights are synthetic by
  necessity — surface the chosen scheme in CLI output.

---

## 6. Fixture ground truth for tests

Useful small graphs inside `build/fixture.srctrldb` (CALL edges), ideal for
unit tests of the new verbs:

```
runChain   → level0, factorial, ping
level0 → level1 → level2 → level3 → level4        (chain)
factorial → factorial                              (self-loop)
ping ⇄ pong                                        (2-cycle)
main [tu_a.cpp#04c3b899] → helper [tu_a.cpp#...]   (TU-local pair)
main [tu_b.cpp#3cfc5d9c] → helper [tu_b.cpp#...]   (second TU, same names)
squarePerimeter → Square::area                     (METHOD callee)
callOverloads → f(int), f(double), f(const int&),  (overloads: distinct
               Accessor::value(), Accessor::value() const   nodes by signature)
useEverything → {Square::ctor, add, anonymousHelper, Square::area,
                 squarePerimeter}  + USAGE → {gGlobalCounter, Point::nx, Color::Green, ...}
unsolved-symbol hub: 3 CALL edges in (from f(const int&), Box::touch, genericFunc)
```

Sanity SQL for the whole graph at a glance:

```sql
SELECT ek.name, COUNT(*) FROM edge e
JOIN (SELECT 1 v, 'MEMBER' name UNION ALL SELECT 2,'TYPE_USAGE'
      UNION ALL SELECT 4,'USAGE' UNION ALL SELECT 8,'CALL'
      UNION ALL SELECT 16,'INHERITANCE' UNION ALL SELECT 32,'OVERRIDE'
      UNION ALL SELECT 256,'INCLUDE' UNION ALL SELECT 2048,'MACRO_USAGE') ek
  ON ek.v = e.type GROUP BY e.type;
```

---

## 7. Open questions (for the lead / next tasks)

1. **Unsolved hub node**: exclude by default from all five algorithms, or
   keep behind an `--include-unsolved` flag? (Recommend: exclude by default;
   it is one node receiving all unresolved references.)
2. **Entry-point seeding** (PageRank / dominator root): accept explicit
   symbol args (existing resolver) plus the `main` heuristic, or require
   explicit seeds always?
3. **Betweenness implementation**: hand-rolled Brandes vs. extra crate —
   hand-rolled keeps the dep tree at just `petgraph`.
4. **Weight scheme for shortest path**: which synthetic default (uniform /
   call-sites / callee-lines)? Could expose `--weight {hops,calls,lines}`.
5. **Slicing presentation**: function-granularity node sets, or expand to
   source-line sets via the occurrence join (richer but noisier)?
6. **Graph scale**: corpora like CCCL/thrust will produce far larger graphs;
   if needed, `petgraph::graphmap` vs `csr::Csr` swap is cheap to defer
   (load path isolated behind a small loader module).

---

## 8. Implementation status (appended after the study was implemented)

Everything above is the study; the study is now implemented. The five
algorithms of §5 live in `rust/src/graph/` (loader, centrality, pagerank,
dominators, slicing, path, render — petgraph-backed, hand-rolled Brandes),
exposed as the Rust-only `symhow graph <verb>` CLI subcommand
(`rust/src/cli/graph_cmd.rs`, JSON/DOT in `rust/src/out/graph_writer.rs`,
schemas `symhow.graph.centrality/1` … `symhow.graph.path/1` in
`docs/JSON_SCHEMA.md`). The open questions above were resolved as follows:
unsolved hub excluded by default with `--include-unsolved` to re-include
(#1); entry-point seeding accepts explicit symbols plus the `main`
heuristic with root-file fallback (#2); Brandes is hand-rolled (#3); the
weight schemes are exactly `--weight {hops,calls,lines}` (#4); slicing stays
function-granularity, with `--lines` exposing per-edge source-line sites
(#5); loading stays behind the loader module for a future CSR swap (#6).
Tests are in `rust/tests/graph_analysis.rs` and `rust/tests/graph_cli.rs`
(full Rust suite: 327 passed). One known caveat discovered in
implementation: personalized PageRank with `--damping 1.0` (no teleport
term) provably oscillates on graphs with dangling nodes and runs to
`max_iterations`; the default damping 0.85 is unaffected.
