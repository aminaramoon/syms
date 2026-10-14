---
name: symhow
description: Use when you need to explore or query the structure of a C++/CUDA codebase through the `symhow` CLI tool — call graphs (callers/callees with depth), class inheritance hierarchies (bases/derived), method overrides, symbol references, definition locations, fuzzy symbol search, file contents/occurrences, or database stats — or to run whole-graph analyses over the indexed call/usage graph (`symhow graph`: betweenness centrality, personalized PageRank, dominator tree, backward slice, weighted shortest path) — or to read/write the per-function knowledge notes attached to it (`symhow query context`, `symhow notes`). Also covers indexing a `compile_commands.json` into a `.srctrldb` database (`symhow index`, parallel/resumable) and keeping it current after code changes (`symhow update`). Trigger on tasks like "find callers of X", "who calls this function", "build/show a call graph", "class hierarchy", "who inherits from / overrides X", "find references to X", "search for a symbol", "what does function X do / what do we know about X", "update the index after my changes", "which functions are load-bearing", "find the bottlenecks / orchestrators / decision hubs", "what is the execution backbone from main", "what does this function depend on", "trace back what can affect X", "what's the happy path / shortest path from A to B", "which functions must have been called before X", working with a `.srctrldb` file or a `notes/*.md` function-notes file, or being asked to run `symhow index` / `symhow update` / `symhow query` / `symhow graph` / `symhow notes`.
---

# symhow — index and query a C++/CUDA codebase

`symhow` is a CLI tool that indexes C++/CUDA source (via libclang, driven by a `compile_commands.json`) into a Sourcetrail-schema SQLite database (`.srctrldb`), then answers structural queries against it: call graphs, inheritance, references, definitions, fuzzy symbol search, file info, and stats. This skill is mostly about the **query** side — once a database exists, this is how an agent reads it.

Project location: `/Users/aaramoon/Workspace/mine/ssearch/symhow`. Binary: `cpp/build/symhow` (built via flox+CMake). Full design/limitations live in `docs/DESIGN.md`, `docs/JSON_SCHEMA.md`, `docs/KNOWN_LIMITATIONS.md` inside that repo — this skill distills what you need to actually drive it; read those docs directly if you need more depth than what's here.

## Setup — every invocation needs the flox environment

`symhow` links against a flox-provided libclang/sqlite, so **always** run it through `flox activate`, from the project directory:

```bash
cd /Users/aaramoon/Workspace/mine/ssearch/symhow
flox activate -- ./cpp/build/symhow <args...>
```

(Or `flox activate` once to get a shell, then run `./cpp/build/symhow ...` directly for the rest of the session — cheaper if you're issuing many commands.)

If `cpp/build/symhow` doesn't exist yet, build it first: `flox activate -- bash -c 'cmake -S cpp -B cpp/build -G Ninja -DCMAKE_BUILD_TYPE=Debug && cmake --build cpp/build -j'`.

Two ready-made databases already exist in this repo for practicing/sanity-checking queries without indexing anything yourself:
- `cpp/build/cucs.srctrldb` — a real, large database (200 TUs, ~20k symbols, ~93k references) from the `cucascade` C++/CUDA project.
- `cpp/build/fixture.srctrldb` — a small, synthetic test database (a tiny class hierarchy, overloads, templates, macros) — good for quick, easy-to-read sanity checks.

## Indexing (creating a database)

```bash
flox activate -- ./cpp/build/symhow index -c /path/to/compile_commands.json -o mydb.srctrldb --force
```

Key flags: `--remap OLD=NEW` (repeatable path-prefix substitution — needed if the CDB was generated in a different location/container than where you're indexing from), `--include GLOB` / `--exclude GLOB` (limit which TUs are processed AND which headers are walked — when using `--include`, list the project's own header dirs too, e.g. `--include '/repo/src/**' --include '/repo/include/**'`), `--exclude-submodules` (skip every git submodule declared in the repo's `.gitmodules` — the quick way to leave vendored dependencies out; their symbols still show up as non-indexed targets when your code references them), `--cuda-mode auto|full|nowrapper|host-only|skip` (CUDA `.cu` files, default `auto` tries progressively degraded parsing strategies), `--limit N` (only process the first N TUs — useful for a quick smoke test on a big project), `--json` (print an `symhow.index-summary/1` JSON object with symbol/reference/error counts instead of just human text), `-v` (repeatable, verbose per-TU progress on stderr). Run `flox activate -- ./cpp/build/symhow index --help` for the full flag list (path remapping, arg sanitizing overrides, resource-dir/toolchain overrides, etc.).

Indexing a real project can take a while (the 200-TU `cucascade` corpus above takes ~100s). Prefer `--limit 5` or a narrow `--include` glob for a first smoke test before indexing everything; on a big multi-core project, add `-j 8` (or higher) to extract TUs concurrently — output is identical regardless of `-j`, only wall time improves.

Ctrl-C is safe: `symhow index` checkpoints every 25 TUs by default (`--commit-every N`), so an interrupted run (exit 130) loses at most the last N TUs' work, not the whole run — just run `symhow update -d <output>` to finish it.

## Keeping a database up to date

If a `.srctrldb` already exists for a repo and only a handful of files changed since it was indexed (e.g. after a `git pull`, or after making a small local edit), use **`symhow update`** instead of re-running `symhow index` from scratch — it re-indexes only what actually changed:

```bash
flox activate -- ./cpp/build/symhow update -d mydb.srctrldb -v
```

No other flags are usually needed: the repo root, baseline commit, `compile_commands.json` path, and every `symhow index` option are all read back out of the database's own provenance. `symhow update` diffs the stored baseline against the current worktree (uncommitted edits included — it's always the worktree that gets indexed, not just `HEAD`), clears exactly the facts the changed files (and anything transitively `#include`ing them) invalidated, and re-indexes only the affected translation units.

When to use `symhow update` vs a fresh `symhow index --force`:
- Use `symhow update` for the common case: a database already exists and only some files changed since.
- Re-index from scratch (`symhow index --force`) if `symhow update` refuses (exit `2` — see below), if the `compile_commands.json` itself changed shape dramatically (e.g. a totally different build config), or if you just don't have an existing database yet.

Useful flags: `--files PATH...` (classify these files as changed directly, skipping git — handy with no repo, or to force-reindex specific files), `--dry-run` (show what would happen without touching the database — run this first if unsure), `--json` (machine-readable `symhow.update-summary/1` summary, see `docs/JSON_SCHEMA.md`).

Exit codes: `0` success, `1` a hard error, `2` refused (no usable provenance, incompatible walker version, or an interrupted `--in-place` run — re-index from scratch in any of these cases), `5` nothing changed since the last run (not an error).

**Symbol/node ids for untouched code are stable across an update.** If a file wasn't touched (directly or transitively via an invalidated `#include`), the ids of the symbols it declares don't change — so cached node ids (e.g. from a previous `symhow query search` result) remain valid after a `symhow update` as long as the relevant file wasn't part of what changed. A symbol whose OWN declaring file gets cleared and re-indexed, however, gets a fresh id (there's no attempt to preserve ids across a real re-index of a file).

## Querying — the main event

```bash
flox activate -- ./cpp/build/symhow query <VERB> <SYMBOL-or-TEXT-or-PATH> -d path/to/db.srctrldb [flags...]
```

`-d/--db` defaults to `$SYMHOW_DB` env var or `./symhow.srctrldb` if omitted — **always pass `-d` explicitly** unless you've set that up.

### The verbs

| Verb | Argument | What it does |
|---|---|---|
| `callers SYMBOL` | symbol | Who calls this function/method, up to `--depth` hops (BFS over CALL edges, reversed) |
| `callees SYMBOL` | symbol | What this function/method calls, up to `--depth` hops |
| `bases SYMBOL` | symbol | Base classes, transitively up to `--depth` |
| `derived SYMBOL` | symbol | Derived classes, transitively up to `--depth` |
| `overrides SYMBOL` | symbol | Methods this one overrides (direct, one hop) |
| `overridden-by SYMBOL` | symbol | Methods that directly override this one (one hop only — NOT transitive, see Gotchas) |
| `members SYMBOL` | symbol | Direct members of a namespace/class (one hop) |
| `neighbors SYMBOL` | symbol | Every edge touching this symbol in either direction, one hop, optionally filtered by `--edge-kind` |
| `references SYMBOL` | symbol | Every place this symbol is referenced (calls, usages, inheritance, includes...) plus its own declaration |
| `definition SYMBOL` | symbol | Where this symbol is defined/declared (prefers the full scope span over just the name token) |
| `search TEXT` | fuzzy text | Fuzzy/subsequence symbol-name search, ranked (like an IDE "go to symbol") |
| `file PATH` | file path | File metadata, optionally `--contents` and/or `--occurrences` (every symbol/reference located in that file) |
| `stats` | — | Database-wide counts (nodes/edges/files by kind, error list) — good first call to sanity-check a DB |

Graph-shaped verbs (`callers callees bases derived members neighbors overrides overridden-by`) all share flags: `--depth N` (default 1, `0` = unlimited — only meaningful for `callers/callees/bases/derived`, the other four are fixed at depth 1), `--max-nodes N` (default 500, caps result size, sets `truncated`), `--kind K` (repeatable, filter returned nodes by kind e.g. `CLASS`, `FUNCTION`), `--edge-kind K` (repeatable, `neighbors` only).

### Addressing a symbol

The `SYMBOL` argument can be:
- An exact qualified name: `'cucascade::memory::reservation_limit_policy'`, `'level0'`, `'Foo::bar'`.
- A literal database node id: `'#8997'` (use this after a `search` or an ambiguous-result listing gives you an id — fastest and unambiguous).
- Passed through fuzzy fallback automatically if no exact match is found (prints "did you mean" suggestions and exits 3 — see Exit codes).

If a name resolves to **more than one** symbol (common! see Gotchas), `symhow` prints a candidate table (id, kind, location) and exits `2`. Narrow with `--kind CLASS` (or whatever kind), `--signature "(int)"` (exact parameter-list match, for disambiguating overloads), or just re-run with `#<id>` from the candidate list. Or pass `--all-matches` to query every candidate at once.

### Text vs `--json` output

Plain text is for humans (indented tree for graph verbs, one-line-per-hit for references/search, key:value for stats). **Always add `--json` when you (the agent) are the consumer** — it's a stable, documented schema; scraping the text format is fragile and unnecessary. Every verb supports `--json`.

### Exit codes (check these, don't just parse stdout)

`0` success · `1` usage/argument error · `2` symbol/file ambiguous (candidates printed to stdout under `--json`, else stdout text table) · `3` symbol/file not found (suggestions printed) · `4` database missing/unreadable/wrong schema version.

## Graph analysis — `symhow graph` (Rust backend only)

`symhow graph <VERB>` runs whole-graph algorithms over the indexed call/usage graph — the layer you want when the question is about the *shape* of the graph ("what's load-bearing", "what's the backbone", "what does X depend on") rather than one symbol's neighborhood. **Rust-only subcommand**: the C++ binary has no `graph` verb — run the Rust binary (`rust/target/debug/symhow`; build it with `flox activate -- bash -c 'cd rust && cargo build'` from the repo root, using the symhow flox env — a parent-workspace flox env lacks `nlohmann/json.hpp` and breaks the vendored writer build).

Grammar: **the verb comes before the flags**.

```bash
flox activate -- ./rust/target/debug/symhow graph VERB -d DB [OPTIONS]
# VERB ∈ {centrality, pagerank, dominators, slice, path}
```

Shared flags: `-d/--db PATH` (or `$SYMHOW_DB`, or `./symhow.srctrldb`), `--json`, `--dot FILE` (also writes a Graphviz DOT file; parent dirs are created — for `centrality`/`pagerank` it renders the subgraph induced by the reported top-N nodes), `-v`, `-q`. Symbols resolve exactly like `symhow query` (qualified/display/leaf name, or `#<id>`); exit codes are the same contract: `0` ok, `1` usage error, `2` ambiguous symbol / entry-point choice, `3` not found, `4` database error.

| Verb | Flags | Question it answers (algorithm) |
|---|---|---|
| `centrality` | `--top N` (default 25, `0` = all), `--include-unsolved` | **Which functions are the orchestrators / decision hubs?** Betweenness centrality (Brandes, unweighted). Scores are raw directed dependency sums, *not* normalized — divide by `stats.normalized_denominator` = `(n-1)(n-2)` if you need 0–1 values. The shared unsolved-symbol hub node is excluded by default (`--include-unsolved` keeps it) because it collects every unresolved reference and distorts the ranking. |
| `pagerank` | `--seed SYMBOL` (repeatable), `--damping F` (default 0.85), `--top N` | **Which load-bearing logic is reachable from the entry points?** Personalized PageRank (scores sum to ~1). No `--seed` → entry-point fallback (`main` functions, then root-file functions). |
| `dominators` | `--entry SYMBOL` | **What is the execution backbone from an entry — which functions run on EVERY path to a node?** Dominator tree of the call graph. No `--entry` → `main` heuristic; when several candidates exist it lists them and exits `2` (pick one with `--entry` or `#<id>`). |
| `slice` | `--criterion SYMBOL` (required), `--lines` | **What does this function depend on — everything that can affect it?** Backward slice: reverse transitive closure over USAGE+CALL edges, function granularity. `--lines` annotates each edge with its source-line sites (text + DOT). |
| `path` | `--from SYMBOL`, `--to SYMBOL` (both required), `--weight hops\|calls\|lines` (default `hops`) | **What's the happy path from A to B?** Weighted shortest path (Dijkstra). An unreachable target prints "no path" and exits **0** — reachability is the answer, not an error. |

Worked examples (verified against `cpp/build/fixture.srctrldb`):

```bash
cd <symhow repo>
# Orchestrators / decision hubs, as JSON
flox activate -- ./rust/target/debug/symhow graph centrality -d cpp/build/fixture.srctrldb --top 10 --json
# Load-bearing logic reachable from the entry points (fallback seeds)
flox activate -- ./rust/target/debug/symhow graph pagerank -d cpp/build/fixture.srctrldb --json
# Execution backbone from one entry point, plus a DOT rendering
flox activate -- ./rust/target/debug/symhow graph dominators -d cpp/build/fixture.srctrldb --entry '#206' --dot /tmp/dom.dot
# Everything a function depends on, with call-site line numbers
flox activate -- ./rust/target/debug/symhow graph slice -d cpp/build/fixture.srctrldb --criterion level2 --lines
# Happy path from A to B, weighted by call-site count
flox activate -- ./rust/target/debug/symhow graph path -d cpp/build/fixture.srctrldb --from runChain --to level0 --weight calls
```

JSON: `--json` emits `symhow.graph.centrality/1`, `symhow.graph.pagerank/1`, `symhow.graph.dominators/1`, `symhow.graph.slice/1`, `symhow.graph.path/1` — same envelope conventions as every other schema (`symhow_version`+`schema` first, `query` echo, `stats`, `truncated`, alphabetical keys). Ambiguous symbols reuse `symhow.ambiguous/1` (exit 2), not-found reuses `symhow.not-found/1` (exit 3). One deviation from the canonical node object: graph node objects carry only `{id, kind, name, qualified_name, signature, display_name?}` — they omit `indexed`/`definition_kind`/`location` (and `depth`/`seed`); join on `id` with `symhow.graph/1`/`symhow.definition/1` output when you need locations. Full field lists in `docs/JSON_SCHEMA.md`.

**Honest approximations — read before trusting any of the five verbs:**
- The graph is the **call + usage graph at symbol granularity** (USAGE edges = field/global accesses, included by default). There are no basic blocks in the schema: `dominators` is *call-graph* dominance, not control-flow dominance, and `slice` is a function-granularity reverse closure that cannot separate reads from writes. Local variables/parameters have no edges at all.
- Weights are **synthetic** (`hops` = 1 per edge, `calls` = the edge's call-site count, `lines` = the callee's body line count) — there is no profile/runtime data in the database, so "shortest" means structurally cheapest, not fastest.
- `pagerank --damping 1.0` with dangling nodes (functions with no outgoing edges) oscillates and runs to `max_iterations` — the teleport term is what guarantees convergence; the default 0.85 is unaffected.

## Real worked examples (verified against this repo's own databases)

```bash
cd /Users/aaramoon/Workspace/mine/ssearch/symhow

# Sanity-check a DB first
flox activate -- ./cpp/build/symhow query stats -d build/cucs.srctrldb --json

# Don't know the exact name? Fuzzy search first.
flox activate -- ./cpp/build/symhow query search 'reslimpol' -d build/cucs.srctrldb --limit 5

# Class hierarchy, unlimited depth
flox activate -- ./cpp/build/symhow query bases 'cucascade::memory::increase_reservation_limit_policy' -d build/cucs.srctrldb --depth 0

# Callers of a method, 2 hops, JSON for programmatic use
flox activate -- ./cpp/build/symhow query callers 'cucascade::memory::reservation_limit_policy::get_policy_name' \
  -d cpp/build/cucs.srctrldb --depth 2 --json

# Everything referencing a type
flox activate -- ./cpp/build/symhow query references 'cucascade::idata_representation' -d build/cucs.srctrldb --json

# Where is it actually defined
flox activate -- ./cpp/build/symhow query definition 'cucascade::memory::reservation_limit_policy' -d build/cucs.srctrldb

# Everything in one file
flox activate -- ./cpp/build/symhow query file /home/aaramoon/workspace/oss/cucs/include/cucascade/memory/memory_reservation.hpp \
  -d cpp/build/cucs.srctrldb --occurrences
```

A resolved-ambiguous example (`Square` the class vs. `Square(double)` its own constructor, both leaf-named "Square" — resolve with `--kind`):
```bash
flox activate -- ./cpp/build/symhow query bases 'Square' -d cpp/build/fixture.srctrldb --kind CLASS --depth 0
```

## JSON schema reference (for programmatic consumption)

Every `--json` output is exactly one JSON object (never an array-wrapped list, never multi-line) starting with `"symhow_version"` and `"schema"`. Schema names, one per verb-family:

| Schema | Used by | Top-level array field |
|---|---|---|
| `symhow.graph/1` | `callers callees bases derived members neighbors overrides overridden-by context` | `nodes[]`, `edges[]` |
| `symhow.graph.centrality/1` | `symhow graph centrality` | `nodes[]` (+ `score`) |
| `symhow.graph.pagerank/1` | `symhow graph pagerank` | `nodes[]` (+ `score`) |
| `symhow.graph.dominators/1` | `symhow graph dominators` | `nodes[]`, `idoms[]` |
| `symhow.graph.slice/1` | `symhow graph slice` | `nodes[]`, `edges[]` |
| `symhow.graph.path/1` | `symhow graph path` | `nodes[]`, `edges[]` (+ `found`) |
| `symhow.references/1` | `references` | `references[]` (+ `nodes[]`) |
| `symhow.definition/1` | `definition` | `locations[]` (+ single `symbol`) |
| `symhow.search/1` | `search` | `hits[]` (each `{score, matched_indices, node}`) |
| `symhow.stats/1` | `stats` | `node_kinds{}`, `edge_kinds{}`, `errors[]` |
| `symhow.file/1` | `file` | `occurrences[]` (if `--occurrences`) |
| `symhow.index-summary/1` | `symhow index --json` | `counts{}` |
| `symhow.ambiguous/1` | any verb, exit code 2 | `candidates[]` |
| `symhow.not-found/1` | any verb, exit code 3 | `suggestions[]` |

**The node object** (appears throughout — `nodes[]`, `hits[].node`, `symbol`, candidates, suggestions):
```json
{ "id": 4711, "kind": "METHOD", "name": "get_policy_name",
  "qualified_name": "cucascade::memory::increase_reservation_limit_policy::get_policy_name",
  "signature": "std::string get_policy_name() const",
  "indexed": true, "definition_kind": "EXPLICIT",
  "depth": 0, "seed": true,
  "location": { "file": "...", "start_line": 164, "start_column": 15,
                "end_line": 164, "end_column": 29, "kind": "TOKEN" } }
```
Use **`qualified_name`** as "the" canonical name field (bare ancestor path, no signature). `name` is just the leaf. `signature` includes params/return type — use it to tell overloads apart. `depth`/`seed` only appear on `symhow.graph/1` nodes. `location` and `definition_kind` are JSON `null` (present key, null value) when the node has none / isn't indexed — not an absent key. `kind` is **always a string** (`"CLASS"`, `"FUNCTION"`, `"CALL"`, etc.), never a raw integer. The `symhow graph` verbs use a **reduced** node object instead: `{id, kind, name, qualified_name, signature, display_name?}` only — no `indexed`/`definition_kind`/`location`/`depth`/`seed` (join on `id` with a `symhow.graph/1`/`symhow.definition/1` result when you need locations).

Full field-by-field docs, every schema's complete shape, and every "invented, not from the DB" convention (see Gotchas below) are in `docs/JSON_SCHEMA.md` in this repo — read it directly if a field's meaning isn't obvious from context.

## Function notes (knowledge that persists across sessions)

`symhow notes` lets you attach durable, git-versioned knowledge (`brief/what/how/pre/post/bugs/suggestions`) to a function/method so the NEXT session (or a different worktree) doesn't have to re-read the code from scratch. Workflow for an agent:

1. **Look before you read.** `symhow query context SYMBOL --json -d DB` returns the symbol plus its callers/callees/overrides in one call, with a `notes` field on every node (the full note object, or `null`). Check the seed's `notes` first.
2. **`notes: null` or `"filled": false`** — no usable note yet. Read the code, then write the note:
   - If `notes.path`/`notes.line` point at an existing block, edit that markdown file directly at that heading — fill in `### what ?`, `### how ?`, the `pre`/`post`/`bugs`/`suggestions` numbered lists, replacing `_TODO_` placeholders. Keep the existing section order and fence format (`### signature` stays a ` ```cpp ` fence); don't touch the `<!-- symhow-hash: ... symhow-commit: ... -->` comment by hand.
   - If there's no block yet, `symhow notes scaffold SYMBOL -d DB [--notes-dir DIR]` creates one (idempotent — safe to re-run), then edit it as above.
   - Run `symhow notes sync -d DB --notes-dir DIR` afterward so the DB cache picks up the edit (`symhow query`/`show` auto-detect an out-of-date cache and warn on stderr, but never write — you still need to run `sync` yourself).
3. **`"stale": true`** — the function's body changed since the note was last verified (hash mismatch). Re-read the current code, verify/update the note's text if the change invalidated it, then run `symhow notes accept SYMBOL -d DB --notes-dir DIR` to re-anchor the `symhow-hash` comment to the current body. `accept` refuses non-canonical markdown (run `symhow notes fmt --notes-dir DIR` first if it does).
4. **Finding review work:** `symhow notes status -d DB --stale --json` lists everything whose body changed since it was last reviewed; `--unfilled` lists scaffolded-but-empty blocks; `--problems` lists matching errors (`unmatched`/`ambiguous`/`orphaned-file`/...).
5. Exit codes worth knowing: `scaffold`/`sync`/`accept`/`status` are 0 on success; `show`/`sync --strict` exit 3 if the symbol/section has no note or there are lint problems; symbol resolution reuses `symhow query`'s own 2 (ambiguous) / 3 (not found) contract.

Never hand-edit the `symhow-hash`/`symhow-commit` comment — it is written only by `scaffold` (initial) and `accept` (re-anchor); editing it yourself desyncs staleness detection from reality.

## Gotchas an agent should know before trusting output blindly

- **A class and its own same-named constructor collide on bare-name lookup.** `symhow query bases Square` is legitimately **ambiguous** (exit 2), not a bug — the class node and its constructor both have leaf name `Square`. Add `--kind CLASS` or use `#<id>`.
- **BFS-based verbs (`callers/callees/bases/derived/...`) always include the seed symbol itself in the result, at `depth: 0`.** "X's bases" as a JSON array literally contains X. Filter to `depth > 0` if you want just the answer set excluding the thing you asked about.
- **`overridden-by` is exactly one hop, not transitive.** If `A::f` is overridden by `B::f` which is itself overridden by `C::f`, `overridden-by A::f` only returns `B::f`. Walk `overridden-by` repeatedly (or use `neighbors`/a manual multi-hop) if you need the full override tree.
- **`references[]` can contain a synthetic `"edge_kind": "DECLARATION"` entry** (with `edge_id: null`) — that's the symbol's own declaration site, not a real edge kind from the database. Every other entry's `edge_kind` is a real one (`CALL`, `INHERITANCE`, ...).
- **`symhow.file/1` occurrences use an invented `"is_local_symbol"` field** to distinguish a local variable/parameter occurrence from a real symbol occurrence — check for the presence of the `"node"` key (real symbol) vs `is_local_symbol: true` (local) vs neither (a reference-token occurrence, e.g. where a call happens, as opposed to where the called thing is declared).
- **Fuzzy `search` scores can be negative** — don't assume "positive = good match"; scores are only meaningful relative to each other within one result set, sorted descending.
- **`neighbors`'s `truncated` field is always `false`**, even if the result is effectively huge — that verb never applies a node cap. Don't rely on it to detect "there's more."
- **`--errors 0` on `stats` does NOT mean "show all errors"** — it's currently treated the same as the default (100). Use a large explicit number if you need more than 100 error rows.
- **`stats.elapsed_ms` (and any `elapsed_ms` field) is real wall-clock time** — don't diff two JSON outputs expecting byte-identical results unless you also account for this field varying.
- Local variables/parameters/template parameters **never appear as call/usage graph edges** — only named, addressable symbols (functions, classes, fields, globals, etc.) do. Don't expect `references`/`callers`/`callees` results for a local variable.
