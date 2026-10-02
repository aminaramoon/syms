---
name: kg
description: Use when you need to explore or query the structure of a C++/CUDA codebase through the `kg` CLI tool — call graphs (callers/callees with depth), class inheritance hierarchies (bases/derived), method overrides, symbol references, definition locations, fuzzy symbol search, file contents/occurrences, or database stats — or to read/write the per-function knowledge notes attached to it (`kg query context`, `kg notes`). Also covers indexing a `compile_commands.json` into a `.srctrldb` database (`kg index`, parallel/resumable) and keeping it current after code changes (`kg update`). Trigger on tasks like "find callers of X", "who calls this function", "build/show a call graph", "class hierarchy", "who inherits from / overrides X", "find references to X", "search for a symbol", "what does function X do / what do we know about X", "update the index after my changes", working with a `.srctrldb` file or a `notes/*.md` function-notes file, or being asked to run `kg index` / `kg update` / `kg query` / `kg notes`.
---

# kg — index and query a C++/CUDA codebase

`kg` is a CLI tool that indexes C++/CUDA source (via libclang, driven by a `compile_commands.json`) into a Sourcetrail-schema SQLite database (`.srctrldb`), then answers structural queries against it: call graphs, inheritance, references, definitions, fuzzy symbol search, file info, and stats. This skill is mostly about the **query** side — once a database exists, this is how an agent reads it.

Project location: `/home/aaramoon/workspace/mine/knowledge_graph/kg`. Binary: `build/kg` (built via flox+CMake). Full design/limitations live in `docs/DESIGN.md`, `docs/JSON_SCHEMA.md`, `docs/KNOWN_LIMITATIONS.md` inside that repo — this skill distills what you need to actually drive it; read those docs directly if you need more depth than what's here.

## Setup — every invocation needs the flox environment

`kg` links against a flox-provided libclang/sqlite, so **always** run it through `flox activate`, from the project directory:

```bash
cd /home/aaramoon/workspace/mine/knowledge_graph/kg
flox activate -- ./build/kg <args...>
```

(Or `flox activate` once to get a shell, then run `./build/kg ...` directly for the rest of the session — cheaper if you're issuing many commands.)

If `build/kg` doesn't exist yet, build it first: `flox activate -- bash -c 'cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug && cmake --build build -j'`.

Two ready-made databases already exist in this repo for practicing/sanity-checking queries without indexing anything yourself:
- `build/cucs.srctrldb` — a real, large database (200 TUs, ~20k symbols, ~93k references) from the `cucascade` C++/CUDA project.
- `build/fixture.srctrldb` — a small, synthetic test database (a tiny class hierarchy, overloads, templates, macros) — good for quick, easy-to-read sanity checks.

## Indexing (creating a database)

```bash
flox activate -- ./build/kg index -c /path/to/compile_commands.json -o mydb.srctrldb --force
```

Key flags: `--remap OLD=NEW` (repeatable path-prefix substitution — needed if the CDB was generated in a different location/container than where you're indexing from), `--include GLOB` / `--exclude GLOB` (limit which TUs are processed AND which headers are walked — when using `--include`, list the project's own header dirs too, e.g. `--include '/repo/src/**' --include '/repo/include/**'`), `--exclude-submodules` (skip every git submodule declared in the repo's `.gitmodules` — the quick way to leave vendored dependencies out; their symbols still show up as non-indexed targets when your code references them), `--cuda-mode auto|full|nowrapper|host-only|skip` (CUDA `.cu` files, default `auto` tries progressively degraded parsing strategies), `--limit N` (only process the first N TUs — useful for a quick smoke test on a big project), `--json` (print an `kg.index-summary/1` JSON object with symbol/reference/error counts instead of just human text), `-v` (repeatable, verbose per-TU progress on stderr). Run `flox activate -- ./build/kg index --help` for the full flag list (path remapping, arg sanitizing overrides, resource-dir/toolchain overrides, etc.).

Indexing a real project can take a while (the 200-TU `cucascade` corpus above takes ~100s). Prefer `--limit 5` or a narrow `--include` glob for a first smoke test before indexing everything; on a big multi-core project, add `-j 8` (or higher) to extract TUs concurrently — output is identical regardless of `-j`, only wall time improves.

Ctrl-C is safe: `kg index` checkpoints every 25 TUs by default (`--commit-every N`), so an interrupted run (exit 130) loses at most the last N TUs' work, not the whole run — just run `kg update -d <output>` to finish it.

## Keeping a database up to date

If a `.srctrldb` already exists for a repo and only a handful of files changed since it was indexed (e.g. after a `git pull`, or after making a small local edit), use **`kg update`** instead of re-running `kg index` from scratch — it re-indexes only what actually changed:

```bash
flox activate -- ./build/kg update -d mydb.srctrldb -v
```

No other flags are usually needed: the repo root, baseline commit, `compile_commands.json` path, and every `kg index` option are all read back out of the database's own provenance. `kg update` diffs the stored baseline against the current worktree (uncommitted edits included — it's always the worktree that gets indexed, not just `HEAD`), clears exactly the facts the changed files (and anything transitively `#include`ing them) invalidated, and re-indexes only the affected translation units.

When to use `kg update` vs a fresh `kg index --force`:
- Use `kg update` for the common case: a database already exists and only some files changed since.
- Re-index from scratch (`kg index --force`) if `kg update` refuses (exit `2` — see below), if the `compile_commands.json` itself changed shape dramatically (e.g. a totally different build config), or if you just don't have an existing database yet.

Useful flags: `--files PATH...` (classify these files as changed directly, skipping git — handy with no repo, or to force-reindex specific files), `--dry-run` (show what would happen without touching the database — run this first if unsure), `--json` (machine-readable `kg.update-summary/1` summary, see `docs/JSON_SCHEMA.md`).

Exit codes: `0` success, `1` a hard error, `2` refused (no usable provenance, incompatible walker version, or an interrupted `--in-place` run — re-index from scratch in any of these cases), `5` nothing changed since the last run (not an error).

**Symbol/node ids for untouched code are stable across an update.** If a file wasn't touched (directly or transitively via an invalidated `#include`), the ids of the symbols it declares don't change — so cached node ids (e.g. from a previous `kg query search` result) remain valid after a `kg update` as long as the relevant file wasn't part of what changed. A symbol whose OWN declaring file gets cleared and re-indexed, however, gets a fresh id (there's no attempt to preserve ids across a real re-index of a file).

## Querying — the main event

```bash
flox activate -- ./build/kg query <VERB> <SYMBOL-or-TEXT-or-PATH> -d path/to/db.srctrldb [flags...]
```

`-d/--db` defaults to `$KG_DB` env var or `./kg.srctrldb` if omitted — **always pass `-d` explicitly** unless you've set that up.

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

If a name resolves to **more than one** symbol (common! see Gotchas), `kg` prints a candidate table (id, kind, location) and exits `2`. Narrow with `--kind CLASS` (or whatever kind), `--signature "(int)"` (exact parameter-list match, for disambiguating overloads), or just re-run with `#<id>` from the candidate list. Or pass `--all-matches` to query every candidate at once.

### Text vs `--json` output

Plain text is for humans (indented tree for graph verbs, one-line-per-hit for references/search, key:value for stats). **Always add `--json` when you (the agent) are the consumer** — it's a stable, documented schema; scraping the text format is fragile and unnecessary. Every verb supports `--json`.

### Exit codes (check these, don't just parse stdout)

`0` success · `1` usage/argument error · `2` symbol/file ambiguous (candidates printed to stdout under `--json`, else stdout text table) · `3` symbol/file not found (suggestions printed) · `4` database missing/unreadable/wrong schema version.

## Real worked examples (verified against this repo's own databases)

```bash
cd /home/aaramoon/workspace/mine/knowledge_graph/kg

# Sanity-check a DB first
flox activate -- ./build/kg query stats -d build/cucs.srctrldb --json

# Don't know the exact name? Fuzzy search first.
flox activate -- ./build/kg query search 'reslimpol' -d build/cucs.srctrldb --limit 5

# Class hierarchy, unlimited depth
flox activate -- ./build/kg query bases 'cucascade::memory::increase_reservation_limit_policy' -d build/cucs.srctrldb --depth 0

# Callers of a method, 2 hops, JSON for programmatic use
flox activate -- ./build/kg query callers 'cucascade::memory::reservation_limit_policy::get_policy_name' \
  -d build/cucs.srctrldb --depth 2 --json

# Everything referencing a type
flox activate -- ./build/kg query references 'cucascade::idata_representation' -d build/cucs.srctrldb --json

# Where is it actually defined
flox activate -- ./build/kg query definition 'cucascade::memory::reservation_limit_policy' -d build/cucs.srctrldb

# Everything in one file
flox activate -- ./build/kg query file /home/aaramoon/workspace/oss/cucs/include/cucascade/memory/memory_reservation.hpp \
  -d build/cucs.srctrldb --occurrences
```

A resolved-ambiguous example (`Square` the class vs. `Square(double)` its own constructor, both leaf-named "Square" — resolve with `--kind`):
```bash
flox activate -- ./build/kg query bases 'Square' -d build/fixture.srctrldb --kind CLASS --depth 0
```

## JSON schema reference (for programmatic consumption)

Every `--json` output is exactly one JSON object (never an array-wrapped list, never multi-line) starting with `"kg_version"` and `"schema"`. Schema names, one per verb-family:

| Schema | Used by | Top-level array field |
|---|---|---|
| `kg.graph/1` | `callers callees bases derived members neighbors overrides overridden-by context` | `nodes[]`, `edges[]` |
| `kg.references/1` | `references` | `references[]` (+ `nodes[]`) |
| `kg.definition/1` | `definition` | `locations[]` (+ single `symbol`) |
| `kg.search/1` | `search` | `hits[]` (each `{score, matched_indices, node}`) |
| `kg.stats/1` | `stats` | `node_kinds{}`, `edge_kinds{}`, `errors[]` |
| `kg.file/1` | `file` | `occurrences[]` (if `--occurrences`) |
| `kg.index-summary/1` | `kg index --json` | `counts{}` |
| `kg.ambiguous/1` | any verb, exit code 2 | `candidates[]` |
| `kg.not-found/1` | any verb, exit code 3 | `suggestions[]` |

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
Use **`qualified_name`** as "the" canonical name field (bare ancestor path, no signature). `name` is just the leaf. `signature` includes params/return type — use it to tell overloads apart. `depth`/`seed` only appear on `kg.graph/1` nodes. `location` and `definition_kind` are JSON `null` (present key, null value) when the node has none / isn't indexed — not an absent key. `kind` is **always a string** (`"CLASS"`, `"FUNCTION"`, `"CALL"`, etc.), never a raw integer.

Full field-by-field docs, every schema's complete shape, and every "invented, not from the DB" convention (see Gotchas below) are in `docs/JSON_SCHEMA.md` in this repo — read it directly if a field's meaning isn't obvious from context.

## Function notes (knowledge that persists across sessions)

`kg notes` lets you attach durable, git-versioned knowledge (`brief/what/how/pre/post/bugs/suggestions`) to a function/method so the NEXT session (or a different worktree) doesn't have to re-read the code from scratch. Workflow for an agent:

1. **Look before you read.** `kg query context SYMBOL --json -d DB` returns the symbol plus its callers/callees/overrides in one call, with a `notes` field on every node (the full note object, or `null`). Check the seed's `notes` first.
2. **`notes: null` or `"filled": false`** — no usable note yet. Read the code, then write the note:
   - If `notes.path`/`notes.line` point at an existing block, edit that markdown file directly at that heading — fill in `### what ?`, `### how ?`, the `pre`/`post`/`bugs`/`suggestions` numbered lists, replacing `_TODO_` placeholders. Keep the existing section order and fence format (`### signature` stays a ` ```cpp ` fence); don't touch the `<!-- kg-hash: ... kg-commit: ... -->` comment by hand.
   - If there's no block yet, `kg notes scaffold SYMBOL -d DB [--notes-dir DIR]` creates one (idempotent — safe to re-run), then edit it as above.
   - Run `kg notes sync -d DB --notes-dir DIR` afterward so the DB cache picks up the edit (`kg query`/`show` auto-detect an out-of-date cache and warn on stderr, but never write — you still need to run `sync` yourself).
3. **`"stale": true`** — the function's body changed since the note was last verified (hash mismatch). Re-read the current code, verify/update the note's text if the change invalidated it, then run `kg notes accept SYMBOL -d DB --notes-dir DIR` to re-anchor the `kg-hash` comment to the current body. `accept` refuses non-canonical markdown (run `kg notes fmt --notes-dir DIR` first if it does).
4. **Finding review work:** `kg notes status -d DB --stale --json` lists everything whose body changed since it was last reviewed; `--unfilled` lists scaffolded-but-empty blocks; `--problems` lists matching errors (`unmatched`/`ambiguous`/`orphaned-file`/...).
5. Exit codes worth knowing: `scaffold`/`sync`/`accept`/`status` are 0 on success; `show`/`sync --strict` exit 3 if the symbol/section has no note or there are lint problems; symbol resolution reuses `kg query`'s own 2 (ambiguous) / 3 (not found) contract.

Never hand-edit the `kg-hash`/`kg-commit` comment — it is written only by `scaffold` (initial) and `accept` (re-anchor); editing it yourself desyncs staleness detection from reality.

## Gotchas an agent should know before trusting output blindly

- **A class and its own same-named constructor collide on bare-name lookup.** `kg query bases Square` is legitimately **ambiguous** (exit 2), not a bug — the class node and its constructor both have leaf name `Square`. Add `--kind CLASS` or use `#<id>`.
- **BFS-based verbs (`callers/callees/bases/derived/...`) always include the seed symbol itself in the result, at `depth: 0`.** "X's bases" as a JSON array literally contains X. Filter to `depth > 0` if you want just the answer set excluding the thing you asked about.
- **`overridden-by` is exactly one hop, not transitive.** If `A::f` is overridden by `B::f` which is itself overridden by `C::f`, `overridden-by A::f` only returns `B::f`. Walk `overridden-by` repeatedly (or use `neighbors`/a manual multi-hop) if you need the full override tree.
- **`references[]` can contain a synthetic `"edge_kind": "DECLARATION"` entry** (with `edge_id: null`) — that's the symbol's own declaration site, not a real edge kind from the database. Every other entry's `edge_kind` is a real one (`CALL`, `INHERITANCE`, ...).
- **`kg.file/1` occurrences use an invented `"is_local_symbol"` field** to distinguish a local variable/parameter occurrence from a real symbol occurrence — check for the presence of the `"node"` key (real symbol) vs `is_local_symbol: true` (local) vs neither (a reference-token occurrence, e.g. where a call happens, as opposed to where the called thing is declared).
- **Fuzzy `search` scores can be negative** — don't assume "positive = good match"; scores are only meaningful relative to each other within one result set, sorted descending.
- **`neighbors`'s `truncated` field is always `false`**, even if the result is effectively huge — that verb never applies a node cap. Don't rely on it to detect "there's more."
- **`--errors 0` on `stats` does NOT mean "show all errors"** — it's currently treated the same as the default (100). Use a large explicit number if you need more than 100 error rows.
- **`stats.elapsed_ms` (and any `elapsed_ms` field) is real wall-clock time** — don't diff two JSON outputs expecting byte-identical results unless you also account for this field varying.
- Local variables/parameters/template parameters **never appear as call/usage graph edges** — only named, addressable symbols (functions, classes, fields, globals, etc.) do. Don't expect `references`/`callers`/`callees` results for a local variable.
