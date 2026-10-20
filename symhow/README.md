# symhow

`symhow` is a CLI tool that indexes C++/CUDA code (via libclang) into a
Sourcetrail-schema-compatible SQLite database (`.srctrldb`), and queries
that database — call graphs, class hierarchies, overrides, references,
definitions, fuzzy symbol/file search, and corpus-wide stats — as either
human-readable text or machine-readable JSON. The Rust backend additionally
exposes `symhow graph <verb>`: whole-graph analysis over the indexed
call/usage graph (betweenness centrality, personalized PageRank, dominator
trees, backward slices, weighted shortest paths).

**Status: feature-complete (all 6 phases done).** `symhow index` is a real
libclang-backed indexer (including CUDA support, with a rung-retry ladder
for `.cu` files), and `symhow query <verb>` is a full read-only query engine
with both text and `--json` output for every verb. The Rust backend adds
`symhow graph <verb>` — whole-graph analysis over the indexed call/usage
graph (Rust-only subcommand, see "Graph analysis" below). See
`docs/KNOWN_LIMITATIONS.md` for known gaps and edge cases, and
`docs/JSON_SCHEMA.md` for the JSON output contract.

The reference implementation is C++ (`cpp/src/`, `cpp/include/symhow/`). A complete,
faithful Rust migration lives in `rust/` — every layer (cli, common, db,
model, index, notes, out, query, update) is ported, with the vendored
SourcetrailDB *writer* remaining C++ behind a small C ABI shim, exactly as
in the C++ build. The two implementations produce logically identical
databases and byte-identical query output (see "The Rust backend" below).

## Requirements

symhow uses [flox](https://flox.dev) to provide a pinned, reproducible
toolchain — gcc, cmake, ninja, libclang, sqlite, nlohmann_json, and Catch2
all come from the project's flox environment (`.flox/`), so no system-wide
install of any of these is required (or expected to be used). Every command
below assumes you're running it through `flox activate` (or inside a shell
already `flox activate`d) from the repository root.

## Building

```sh
cd symhow
flox activate -- bash -c '
  cmake -S cpp -B cpp/build -G Ninja -DCMAKE_BUILD_TYPE=Debug
  cmake --build cpp/build -j
'
```

Or, using the checked-in CMake presets (they run from the `cpp/` directory):

```sh
flox activate -- bash -c 'cd cpp && cmake --preset dev && cmake --build --preset dev'
```

A `release` preset (optimized, tests off) is also available:
`cd cpp && cmake --preset release && cmake --build --preset release`.

The resulting `symhow` binary lands at `cpp/build/symhow` (or `cpp/build-release/symhow` for the
release preset).

```sh
flox activate -- ./cpp/build/symhow --version
flox activate -- ./cpp/build/symhow --help
```

## Usage tour

### Indexing

Point `symhow index` at any `compile_commands.json` (the standard clang
compilation database format — CMake produces one automatically with
`CMAKE_EXPORT_COMPILE_COMMANDS=ON`, already the default for this project's
own build):

```sh
flox activate -- ./cpp/build/symhow index \
  -c /path/to/compile_commands.json \
  -o out.srctrldb --force -v
```

Useful flags: `--remap OLD=NEW` (literal path substring remap, for a corpus
compiled in a container/CI path different from where you're indexing it
from), `--include`/`--exclude GLOB` (filter which TUs get processed; also
gates which headers are walked, so list your own `include/**` when using
`--include`), `--exclude-submodules` (exclude every git submodule declared in
the repo's `.gitmodules` — the easy way to skip vendored dependencies),
`--cuda-mode auto|full|nowrapper|host-only|skip` (how to handle `.cu`
files — `auto`, the default, retries a `.cu` TU across progressively more
permissive rungs until one succeeds), `--json` (also print a
`symhow.index-summary/1` JSON object to stdout — see `docs/JSON_SCHEMA.md`),
and `--dry-run` (print each TU's fully-sanitized clang argv and exit,
without actually indexing — useful for debugging a compile-command that
won't parse).

`-j N`/`--parallel N` extracts N TUs concurrently (0 = use every CPU;
default is 1, fully sequential). The output is byte-for-byte
**deterministic regardless of N** — node ids, `symhow_tu` rows, and progress
lines all come out identical to a sequential run, because a single
reorder-buffered consumer thread still feeds the one SQLite writer in
strict `compile_commands.json` order; only wall-clock time changes. Peak
memory scales roughly with N (≈ N × largest TU's AST + fact batch), so pick
N based on available RAM as well as CPU count — `-j 8` is a reasonable
default on a big multi-core box. `symhow update` accepts the same `-j`/
`--parallel` flag for its re-index step.

By default (no `-q`), `symhow index` prints one right-aligned `[ k/N] Ts  status
path` line per TU as it completes, plus a periodic one-line running summary
(`symhow index: 120/975 done, 12.3 TU/min, elapsed 9m41s, eta ~68m`) every ~10
TUs or 5s. `-q` drops the per-TU lines but keeps that summary (now every
~60s) — the useful mode for a long unattended run; `-qq` silences
everything except the final summary line. `-v` is unaffected and still adds
its own extra detail (collision warnings, etc).

**Ctrl-C is safe.** `symhow index` checkpoints every 25 consumed TUs by default
(`--commit-every N`, `0` disables checkpointing entirely and matches the
pre-checkpointing behavior of one transaction for the whole run) — a
SIGINT/SIGTERM (or a crash) loses at most the TUs since the last
checkpoint, never the whole run. An interrupted run exits 130 and prints
`symhow index: interrupted after K/N TUs — completed work is saved; resume
with: symhow update -d <output>`; running that `symhow update` finishes exactly the
untracked remainder and produces a database logically identical to an
uninterrupted `symhow index`. `symhow query` still works against an in-progress
database (read-only), with a one-line stderr notice
(`index incomplete (interrupted) — run `symhow update` to finish`, suppressed
by `-q`/`--json`). `symhow index` itself refuses (exit 2) to overwrite an
in-progress output without `--force`, pointing at `symhow update` instead.

### Updating

Once a database exists, `symhow update` re-indexes only what changed since the
last `symhow index`/`symhow update`, instead of re-indexing the whole corpus:

```sh
flox activate -- ./cpp/build/symhow update -d out.srctrldb -v
```

Everything else — the git repo root, the baseline commit, the
`compile_commands.json` path, and every `symhow index` option (`--remap`,
`--include`/`--exclude`, `--cuda-mode`, ...) — is read back out of
`out.srctrldb`'s own provenance metadata (written by `symhow index` at indexing
time), so a bare `symhow update -d out.srctrldb` is normally enough. It diffs
the stored baseline commit against the current worktree (or `--to REV` for
two fixed commits), clears exactly the facts those changed files (and
anything that transitively `#include`s them) invalidated, and re-indexes
only the affected translation units — plus any TU whose previous run was
`partial`/`failed` (tracked per-TU in `symhow_tu.status`), so a flaky parse
retries automatically without re-running the whole corpus.

**It's always the worktree that gets indexed, not a commit.** `symhow update`
(no `--to`) diffs the stored baseline against `HEAD` *and* folds in the
current `git status` — so uncommitted edits are picked up and re-indexed
just like a committed change would be, and a database `symhow update` produced
this way can legitimately be logically ahead of `HEAD` (it reflects
whatever is on disk right now). `symhow_dirty_paths` (stored in the database)
remembers which paths were dirty at the last run, so reverting an
uncommitted edit still triggers one more re-index of that file (to bring
it back in sync with the now-clean worktree) rather than silently leaving
stale facts behind.

Repeated updates of the **same uncommitted bytes** now skip parsing: symhow
stores SHA-256 fingerprints of libclang's actual source/header buffers and
checks git-reported changes against those snapshots. Further edits and
reverts still invalidate affected TUs, even when file size/mtime are
unchanged. Compiler-argument changes, failed/partial-TU retries, explicit
revisions, and `--files` are never suppressed by content matching. Older
databases without fingerprints remain updateable and acquire fingerprints
as their files are re-indexed. Path normalization is cached within each
update, while retaining symlink resolution and original stored spellings.

Useful flags: `--files PATH...` (classify these files as changed directly,
skipping git entirely — handy for scripting or when there's no repo),
`--to REV` (diff against a specific commit instead of the worktree),
`--dry-run` (show what would be cleared/re-indexed without changing
anything, no database is written), `--in-place` (modify the database
directly instead of the default temp-copy + atomic rename — faster, but an
interrupted run leaves the database unusable until re-indexed from
scratch), `--json` (a `symhow.update-summary/1` object — see
`docs/JSON_SCHEMA.md`).

Exit codes: `0` success (including `--dry-run`), `1` a hard error (e.g.
can't read the compile database), `2` refused — a database with no
provenance at all (never indexed by this `symhow`, or by the real Sourcetrail
indexer — such a database can't be updated, only re-indexed from scratch
with `symhow index`), one written by an incompatible walker version, a
pre-Phase-4 `symhow_tu` table with no `status` column, one an interrupted
`--in-place` run left half-updated, or `--system-file-rows=all` (not
supported incrementally) — and `5` nothing changed since the last run (the
index facts are unchanged, though provenance may be refreshed; this is not an error). See
`docs/KNOWN_LIMITATIONS.md`'s Phase 3/4 sections for the full invalidation
algorithm.

### Querying

Every verb accepts `-d/--db PATH` (default `$SYMHOW_DB` or `./symhow.srctrldb`) and
`--json` (see `docs/JSON_SCHEMA.md` for the exact schema each verb emits).
A few examples, run against a database indexed from the
[cuCascade](https://github.com/nvidia/cuCascade) corpus (a GPU memory
management library — real-world C++20/CUDA code, used as symhow's own
integration-test corpus; see "Testing" below):

```sh
# Corpus-wide stats: node/edge counts by kind, file counts, recorded errors.
flox activate -- ./cpp/build/symhow query stats -d cucs.srctrldb

# Who derives from a base class?
flox activate -- ./cpp/build/symhow query derived 'cucascade::memory::reservation_limit_policy' \
  -d cucs.srctrldb --depth 0
# -> cucascade::memory::ignore_reservation_limit_policy
#    cucascade::memory::fail_reservation_limit_policy
#    cucascade::memory::increase_reservation_limit_policy

# Who calls a given function, two hops out, as JSON?
flox activate -- ./cpp/build/symhow query callers 'cucascade::data_batch::make' \
  -d cucs.srctrldb --depth 2 --json

# Fuzzy symbol search.
flox activate -- ./cpp/build/symhow query search reslimpol -d cucs.srctrldb --kind CLASS

# Where is a file, what does it look like, what's in it?
flox activate -- ./cpp/build/symhow query file 'memory_reservation.hpp' \
  -d cucs.srctrldb --occurrences
```

The full verb list: `callers callees bases derived overrides overridden-by
members neighbors references definition search file stats`. Run
`symhow query --help` (or `symhow --help`) for the complete flag reference.

### Graph analysis (Rust backend only)

`symhow graph <VERB>` runs whole-graph algorithms over the indexed call/usage
graph — every FUNCTION/METHOD node plus every CALL edge, with USAGE edges
(field/global accesses, carrying their source-line sites) on top, and the
shared unsolved-symbol hub node excluded. This is the layer for questions
about the *shape* of the graph rather than one symbol's neighborhood, and it
is implemented **only in the Rust backend** (there is no C++ `Cli.cpp`
counterpart — the C++ binary rejects `graph`): run
`./rust/target/debug/symhow graph ...` instead of `./cpp/build/symhow ...`. The
verb comes **before** the flags.

```sh
# Which functions are the orchestrators / decision hubs?
# (Brandes betweenness; raw dependency sums — divide by
# stats.normalized_denominator = (n-1)(n-2) for a 0-1 value)
flox activate -- ./rust/target/debug/symhow graph centrality \
  -d cpp/build/fixture.srctrldb --top 10 --json

# Which load-bearing logic is reachable from the entry points?
# (personalized PageRank; --seed is repeatable, default seeds are the
# `main` functions then root-file functions)
flox activate -- ./rust/target/debug/symhow graph pagerank \
  -d cpp/build/fixture.srctrldb --seed runChain --top 10

# What is the execution backbone from an entry point — which functions
# run on EVERY path to a node? (call-graph dominator tree; --entry
# defaults to the `main` heuristic — if several `main` candidates exist
# it lists them and exits 2, pick one with --entry '#<id>')
flox activate -- ./rust/target/debug/symhow graph dominators \
  -d cpp/build/fixture.srctrldb --entry '#206' --dot /tmp/dominators.dot

# What does this function depend on — everything that can affect it?
# (backward slice over USAGE+CALL edges, function granularity;
# --lines annotates each edge with its source-line sites)
flox activate -- ./rust/target/debug/symhow graph slice \
  -d cpp/build/fixture.srctrldb --criterion level2 --lines

# What's the happy path from A to B? (weighted shortest path;
# --weight hops|calls|lines, default hops)
flox activate -- ./rust/target/debug/symhow graph path \
  -d cpp/build/fixture.srctrldb --from runChain --to level0 --weight calls
```

Shared flags: `-d/--db PATH` (default `$SYMHOW_DB` or `./symhow.srctrldb`),
`--json` (schemas `symhow.graph.centrality/1`, `symhow.graph.pagerank/1`,
`symhow.graph.dominators/1`, `symhow.graph.slice/1`,
`symhow.graph.path/1` — see `docs/JSON_SCHEMA.md`; ambiguous symbols reuse
`symhow.ambiguous/1`, not-found `symhow.not-found/1`), `--dot FILE` (also
writes a Graphviz DOT rendering, creating parent dirs; for
`centrality`/`pagerank` it is the subgraph induced by the reported top-N
nodes), `-v`, `-q`. Exit codes follow the shared contract: `0` ok, `1` usage
error, `2` ambiguous symbol / entry-point choice, `3` not found, `4`
database error. An unreachable `path` target is **not** an error — it prints
"no path" and exits `0`, because reachability is the answer.

Honest approximations to keep in mind when reading the results: the graph is
the call + usage graph at **symbol granularity** (no basic blocks exist in
the schema, so `dominators` is call-graph dominance, not control-flow
dominance, and `slice` cannot separate reads from writes — local variables
have no edges at all); `path` weights are **synthetic** structural measures
(hops / call-site counts / callee body lines), not profile data; and
`pagerank --damping 1.0` oscillates on graphs with dangling nodes and runs
to `max_iterations` (the default 0.85 is unaffected). The algorithms and
their feasibility study live in `docs/GRAPH_SCHEMA_NOTES.md`.

### Notes

`symhow notes` attaches durable, reviewable function-level knowledge (brief /
what / how / signature / pre-conditions / post-conditions / bugs /
suggestions) to symbols, stored as markdown in git so it's versioned,
diffable, and shared across worktrees/sessions. See `docs/NOTES_DESIGN.md`
for the full design and markdown format.

Phase 1 shipped the parser/renderer (`symhow_notes` library, `cpp/include/symhow/notes/`)
and a DB-free lint:

```sh
flox activate -- ./cpp/build/symhow notes lint --notes-dir notes/ --strict --json
```

Phase 2 (this codebase) adds the DB-backed cache and read paths: a
`symhow_note*` cache schema in the `.srctrldb` (rebuilt from the markdown, never
the source of truth itself), matched to symbols by qualified name +
signature, with automatic staleness tracking (a hash of each symbol's
current definition body) and an overlay so querying never has to write the
DB even when a note file changed on disk since the last sync:

```sh
flox activate -- ./cpp/build/symhow notes sync -d symhow.srctrldb --notes-dir notes/ --json
flox activate -- ./cpp/build/symhow notes show -d symhow.srctrldb some::qualified::name --section what
flox activate -- ./cpp/build/symhow notes status -d symhow.srctrldb --stale --json
flox activate -- ./cpp/build/symhow notes symbols -d symhow.srctrldb --json   # feeds cpp/scripts/symhow-notes-fake.py --symbols-json
```

`cpp/scripts/symhow-notes-fake.py` generates deterministic fake notes trees, either
from a bare JSON symbol list or directly from `symhow notes symbols --json`'s
own output; `--stale-ratio`/`--drop-hash-ratio` inject deterministic
staleness/unanchored-note scenarios for testing.

Phase 3 (this codebase) adds writing: `symhow notes scaffold` creates
placeholder blocks (idempotent and non-destructive -- existing blocks are
never rewritten), `symhow notes accept` re-anchors a note's `symhow-hash`/
`symhow-commit` after reviewing a change, `symhow notes fmt` is a plain canonical
re-render (`--check` for CI), and `symhow update` re-syncs notes automatically
at the end of every run so staleness tracks the code without a separate
step:

```sh
flox activate -- ./cpp/build/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ --all --json
flox activate -- ./cpp/build/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ --class my::Widget
flox activate -- ./cpp/build/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ some::qualified::function
flox activate -- ./cpp/build/symhow notes accept -d symhow.srctrldb some::qualified::function --json
flox activate -- ./cpp/build/symhow notes fmt --notes-dir notes/ --check
flox activate -- ./cpp/build/symhow index -c compile_commands.json -o symhow.srctrldb --notes-dir notes/ --force
flox activate -- ./cpp/build/symhow update -d symhow.srctrldb --json   # re-indexes, then re-syncs notes and reports newly-stale
```

The realistic demo/test path for a fresh notes tree is `scaffold --all` (real
symbols, real hashes) → `python3 cpp/scripts/symhow-notes-fake.py --fill-existing
notes/ --seed N` (fills `_TODO_` placeholders in place, preserving
headings/signatures/hashes byte-for-byte) → `sync`.

Phase 4 (final, this codebase) embeds notes directly into the call graph and
proves they travel with git, independent of any one `.srctrldb`:

```sh
flox activate -- ./cpp/build/symhow query context 'my::qualified::function' -d symhow.srctrldb --json
flox activate -- ./cpp/build/symhow query context 'my::qualified::function' -d symhow.srctrldb --notes-full   # text mode, full note body under each node
flox activate -- ./cpp/build/symhow query context 'my::qualified::function' -d symhow.srctrldb --no-notes      # omit the "notes" field entirely
flox activate -- ./cpp/build/symhow dbdiff a.srctrldb b.srctrldb --only note                                # compare ONLY the notes cache
```

`symhow query context SYMBOL` returns the seed plus its callers/callees (to
`--depth`), one-hop overrides/overridden-by, and (for a CLASS/STRUCT seed)
one-hop bases/derived -- the exact same `symhow.graph/1` envelope every other
graph verb uses, with one addition: every node object gets a `"notes"` field
(the full note object, or `null`), sourced from the same overlay-aware read
path `symhow notes show`/`status` use (a notes file changed on disk since the
last `sync` prints the usual "notes cache out of date" hint to stderr;
`context` never writes the DB). This is the intended entry point for an
agent picking up a function cold: one call gets the code's shape AND
whatever knowledge was already written down about it and its neighbors --
see `.claude/skills/symhow/SKILL.md`'s "Function notes" section for the
read-a-note / write-a-note / accept-a-stale-note workflow.

`symhow dbdiff` also grew a `note`/`note_problem` category (from `symhow_note`/
`symhow_note_problem`, keyed by `serialized_name` -- never by `node.id` or a
file path) and a `--only CATEGORY` flag to restrict a diff to just one
category -- used by the `notes_cross_worktree` test (`ctest -R
notes_cross_worktree`) to prove that two independently-`symhow index`d clones of
the same git history end up with an identical notes cache even though their
node/edge/location rows (which embed each worktree's own absolute paths)
legitimately do not.

## Layout

- `cpp/cmake/` — CMake helper modules (`FindLibClang.cmake`, `SymhowWarnings.cmake`, `SymhowFixtures.cmake`).
- `cpp/third_party/sourcetraildb/` — vendored (Apache-2.0) SourcetrailDB core writer library. See `PROVENANCE.md` there.
- `cpp/include/symhow/`, `cpp/src/` — symhow's C++ reference implementation, organized by module: `common`, `model`, `index`, `db`, `query`, `out`, `cli`, `notes`.
- `rust/` — the Rust backend: a complete, faithful migration of the C++ implementation. See "The Rust backend" below.
- `cpp/scripts/symhow-notes-fake.py` — deterministic fake `symhow notes` markdown generator (see `docs/NOTES_DESIGN.md`).
- `cpp/test/unit/`, `cpp/test/integration/`, `cpp/test/fixtures/` — Catch2-based tests (see "Testing" below).
- `docs/JSON_SCHEMA.md` — the `--json` output contract for every `symhow index`/`symhow query`/`symhow notes`/`symhow graph` invocation.
- `docs/KNOWN_LIMITATIONS.md` — known gaps, edge cases, and implementation notes, organized by phase.
- `docs/NOTES_DESIGN.md` — the `symhow notes` design (markdown format, cache schema, CLI, phases).
- `docs/GRAPH_SCHEMA_NOTES.md` — the `.srctrldb` schema study behind `symhow graph` (extraction SQL, per-algorithm feasibility, fixture ground truth).

## Testing

The default test suite (89 original + several Phase 6 additions, all
hermetic — no external corpus or CUDA toolkit required) runs via:

```sh
flox activate -- ctest --test-dir cpp/build --output-on-failure -LE integration
```

A separate, `"integration"`-labeled pair of tests exercises the full
pipeline against a real ~200-translation-unit C++/CUDA corpus
(`/home/aaramoon/workspace/oss/cucs` by default, overridable via the
`SYMHOW_CUCS_DIR` CMake cache variable): `cucs_integration` (a fresh `symhow index`
of the whole corpus) and `cucs_update_integration` (`symhow update` over a real
commit range in that same corpus, inside a throwaway clone — never the
real checkout — verified against a fresh re-index via `symhow dbdiff`). Both are
excluded from the default run above and skip themselves (CTest `SKIPPED`,
not `FAILED`) on any machine that doesn't have the corpus (or `git`/
`sqlite3`) available:

```sh
flox activate -- ctest --test-dir cpp/build --output-on-failure -L integration
```

`cucs_update_integration` is also labeled `"update"`, alongside the
synthetic-fixture `update_scenario` test, so `ctest -L update` runs both:

```sh
flox activate -- ctest --test-dir cpp/build --output-on-failure -L update
```

`cucs_update_integration` defaults to the commit range `HEAD~1..HEAD` in
the cucs corpus; override with `SYMHOW_CUCS_UPDATE_FROM` (read directly by the
test script at run time, so no reconfigure is needed) for a bigger range:

```sh
SYMHOW_CUCS_UPDATE_FROM=HEAD~5 flox activate -- ctest --test-dir cpp/build --output-on-failure -R cucs_update_integration
```

Both real-corpus update runs take roughly 4 minutes (three full indexes of
the corpus plus the update itself) and print a timing/equivalence summary
at the end.

What `cucs_update_integration` asserts about equivalence, in order:

1. Every garbage-collection predicate `symhow update` deletes by matches **0
   rows on the freshly indexed HEAD database** — the rules are proven
   non-destructive against a full `symhow index`, not just assumed to be.
2. A strict, no-exclusions `symhow dbdiff` of the updated database against the
   fresh one: it may **never** lose a row a fresh index has, and any
   surviving difference must be an occurrence `location` (never a node,
   edge, symbol, file or TU row).
3. `symhow dbdiff --ignore shared-header-locations`, which must be exactly
   empty. That flag drops occurrence locations inside multiply-included
   headers, and nothing else; it exists for one inherent limitation —
   template-instantiation facts land in a shared header's text but belong
   to the TU that instantiated them, and symhow's fact provenance is per-file,
   not per-TU. See `docs/KNOWN_LIMITATIONS.md`'s Phase 5 section for the
   measurements behind this.
4. The single-file speed run's `symhow dbdiff` is checked with **no**
   `--ignore` at all and must be exactly equivalent.

## The Rust backend

`rust/` contains a complete, faithful migration of the C++ implementation
to modern Rust (edition 2021), mirroring the C++ module structure
one-to-one:

| Rust module (under `rust/`) | C++ counterpart (under `cpp/`) |
|---|---|
| `rust/src/common/` (error, log, content_hash, paths, glob, process, shell) | `cpp/src/common/` |
| `rust/src/db/` (database, writable_db, meta, enums, name_decode) — via `rusqlite` | `cpp/src/db/` (raw sqlite3) |
| `rust/src/model/` (enums, facts, fact_sink incl. `BatchSink`/`IndexBackend`) | `cpp/src/model/` |
| `rust/src/index/` (compile_db, arg_sanitizer, cuda_support, progress, provenance, file_registry, clangutil, name_builder, tu_parser, ast_walker — via `clang-sys` — srctrldb_backend, index_driver) | `cpp/src/index/` |
| `rust/src/query/` (query_engine, bfs, fuzzy_matcher, symbol_resolver, results) | `cpp/src/query/` |
| `rust/src/graph/` (loader, centrality, pagerank, dominators, slicing, path, render) | — (Rust-only; no C++ counterpart — see "Graph analysis" above) |
| `rust/src/out/` (text_writer, json_writer, graph_writer) | `cpp/src/out/` (graph_writer is Rust-only) |
| `rust/src/notes/` (markdown, body_hash, notes_store, scaffold, sync, note_model) | `cpp/src/notes/` |
| `rust/src/update/` (git, git_diff, file_matcher, snapshot, invalidator, update_driver) | `cpp/src/update/` |
| `rust/src/cli/` (mod, index_cmd, query_cmd, update_cmd, notes_cmd, dbdiff_cmd, graph_cmd) | `cpp/src/cli/` (graph_cmd is Rust-only) |

The **only** C++ code the Rust binary links is the vendored
SourcetrailDB writer (`cpp/third_party/sourcetraildb/`, which stays C++,
untouched, as in the C++ build): `build.rs` compiles it together with a
~200-line C ABI shim (`rust/ffi/symhow_srctrldb_bridge.cpp`) that the
Rust indexer drives through `src/srctrldb.rs`. Reads and every
symhow-private side table are pure Rust (`rusqlite`, linking the same
sqlite3 the flox env provides).

### Building and testing the Rust backend

```sh
flox activate -- bash -c 'cd rust && cargo build && cargo test'
flox activate -- ./rust/target/debug/symhow --version
# symhow 0.1.0, libclang 21.1.8, sqlite 3.51.2, srctrldb v4.db25.p1
```

The flox env provides the Rust toolchain (cargo/rustc/rustfmt/clippy),
libclang (for `clang-sys`'s runtime loading, via `LIBCLANG_PATH`), and
sqlite3 (linked by both `rusqlite` and the C++ writer shim). Run cargo with
the **symhow** flox env active (as above, or `flox activate -d symhow` from
`rust/`) — a parent-workspace flox env lacks `nlohmann/json.hpp`, which the
vendored SourcetrailDB writer's build needs, and breaks the build.

The Rust suite (327 tests) ports the Catch2 cases layer by layer —
including the fixture-driven ones, which run against the same
`cpp/build/fixture.srctrldb` the C++ suite builds (`SYMHOW_FIXTURE_DB`, default
`../cpp/build/fixture.srctrldb`, and `SYMHOW_FIXTURE_CDB`) — plus an
end-to-end **parity test** (`tests/parity_fixture.rs`) that indexes the
fixture CDB with the Rust pipeline and asserts the result is logically
identical to the C++-built database.

### The parity harness

`rust/parity.sh` is the full C++-vs-Rust proof. It builds both binaries,
indexes the same `compile_commands.json` with each, then:

1. Logically diffs the two `.srctrldb` files (`rust/examples/
   parity_compare.rs`, built on the same `take_snapshot`/`diff_snapshots`
   core as `symhow dbdiff` — id-independent, multiset-exact across all 11
   categories), and
2. Byte-diffs `query --json` output for a battery of verbs (normalizing
   only the echoed db path and `elapsed_ms` timing).

```sh
flox activate -- ./rust/parity.sh   # optionally pass a different CDB
```

Current state: **green** — the two implementations produce logically
identical databases and byte-identical query output on the synthetic
fixture corpus.

## License

symhow's own code is licensed under the Apache License, Version 2.0 (see
`LICENSE`). The vendored SourcetrailDB core writer library
(`cpp/third_party/sourcetraildb/`) is also Apache-2.0, unmodified from upstream.
symhow's own indexing and query-engine logic is an original, clean-room
implementation, informed by (but not derived from) the externally observable
behavior of the Sourcetrail application (which is itself GPLv3-licensed) —
no GPLv3 source was viewed, copied, or otherwise used. See `NOTICE` for the
full third-party attributions and clean-room statement.
