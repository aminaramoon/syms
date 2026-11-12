# symhow

`symhow` is a CLI tool that indexes C++/CUDA code (via libclang) into a
Sourcetrail-schema-compatible SQLite database (`.srctrldb`), and queries
that database — call graphs, class hierarchies, overrides, references,
definitions, fuzzy symbol/file search, and corpus-wide stats — as either
human-readable text or machine-readable JSON. It also
exposes `symhow graph <verb>`: whole-graph analysis over the indexed
call/usage graph (betweenness centrality, personalized PageRank, dominator
trees, backward slices, weighted shortest paths).

**Status: feature-complete (all 6 phases done).** `symhow index` is a real
libclang-backed indexer (including CUDA support, with a rung-retry ladder
for `.cu` files), and `symhow query <verb>` is a full read-only query engine
with both text and `--json` output for every verb. It also provides
`symhow graph <verb>` — whole-graph analysis over the indexed call/usage
graph (see "Graph analysis" below). See
`docs/KNOWN_LIMITATIONS.md` for known gaps and edge cases, and
`docs/JSON_SCHEMA.md` for the JSON output contract.

The client is implemented in Rust under `rust/`. The only C++ code it links
is the vendored SourcetrailDB *writer*, compiled by `rust/build.rs` behind a
small C ABI shim. Database reads, indexing orchestration, queries, graph
analysis, notes, and updates are implemented in Rust.

## Requirements

symhow uses [flox](https://flox.dev) to provide a pinned, reproducible
toolchain — Rust (cargo/rustc/rustfmt/clippy), gcc (for the vendored writer),
libclang, sqlite, and nlohmann_json, plus cmake and ninja for test fixtures,
all come from the project's flox environment (`.flox/`), so no system-wide
install of any of these is required (or expected to be used). Every command
below assumes you're running it through `flox activate` (or inside a shell
already `flox activate`d) from the repository root.

## Building

Run from the `symhow/` project directory, using its flox environment:

```sh
cd symhow
flox activate -- bash -c 'cd rust && cargo build --release'
flox activate -- ./rust/target/release/symhow --version
flox activate -- ./rust/target/release/symhow --help
```

The release binary is `rust/target/release/symhow`. For a debug build, use
`flox activate -- bash -c 'cd rust && cargo build'`; the binary is then
`rust/target/debug/symhow`. Cargo compiles the vendored C++ writer through
`rust/build.rs` and the `cc` crate; no CMake client build is needed.

Use the **symhow** flox environment, not a parent-workspace environment:
the vendored writer needs flox's `nlohmann/json.hpp`, and libclang is loaded
via `LIBCLANG_PATH`.

## Usage tour

### Indexing

Point `symhow index` at any `compile_commands.json` (the standard clang
compilation database format — CMake produces one automatically with
`CMAKE_EXPORT_COMPILE_COMMANDS=ON`, as used by the synthetic test fixture):

```sh
flox activate -- ./rust/target/release/symhow index \
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

### Moving a database to another checkout or machine

New databases store project source paths **relative to a source root**, rather
than baking the indexing machine's absolute checkout path into every location.
The root defaults to the discovered Git
repository root, or the directory containing `compile_commands.json` outside
Git. Pass `index --repo /path/to/project` to select a root explicitly (also
supported for non-Git projects). Files outside that root, such as system and
external dependency headers, retain absolute paths and are not relocated.

After copying the database and checking out the same source tree elsewhere:

```sh
symhow --rebind /new/path/to/project -d out.srctrldb
symhow query definition 'my::function' -d out.srctrldb --json

# Or rebind and query in one invocation:
symhow query definition 'my::function' -d out.srctrldb --rebind /new/path/to/project
```

`--rebind DIR` **persists** the new root in the database; subsequent query,
graph, notes, and update commands use it without repeating the option.
Query locations are resolved absolute paths under the current root, and
`query file` accepts either the root-relative path or its resolved absolute
path. Rebinding does not re-index or verify that the new checkout's source
bytes match the indexed version; use the same revision for accurate locations.
It requires an existing directory and a writable database, and refuses active
updates or unresolved SQLite journals/WALs. Supply `-d/--db` explicitly when
rebinding. With a command, rebinding
happens before that command runs;
`--help` does not change the binding. `index`, `dbdiff`, and database-independent
`notes lint`/`notes fmt` cannot be combined with `--rebind`.

Rebinding also relocates in-root compile-database/notes metadata and records
component-aware root mappings for subsequent updates when `compile_commands.json`
still contains old checkout paths. These mappings are separate from explicit
`--remap` options and also accept compilation databases regenerated at the new
root. Roots are canonicalized; if compiler arguments use a separate symlink
alias for the checkout, pass `--remap /alias=/canonical/root` when indexing so
those arguments remain updateable after the alias disappears.
External dependency/toolchain paths still
need to exist on the target machine or be explicitly overridden when updating.
The `file.path` and `symhow_tu.path` rows remain relative after an update.

Older databases without the relative-path format marker remain readable and
updateable as before. `--rebind` refuses them with re-index guidance: their
path-derived symbol identities cannot safely be converted by replacing a path
prefix. Re-index once with this version to obtain a portable database.

### Updating

Once a database exists, `symhow update` re-indexes only what changed since the
last `symhow index`/`symhow update`, instead of re-indexing the whole corpus:

```sh
flox activate -- ./rust/target/release/symhow update -d out.srctrldb -v
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
flox activate -- ./rust/target/release/symhow query stats -d cucs.srctrldb

# Who derives from a base class?
flox activate -- ./rust/target/release/symhow query derived 'cucascade::memory::reservation_limit_policy' \
  -d cucs.srctrldb --depth 0
# -> cucascade::memory::ignore_reservation_limit_policy
#    cucascade::memory::fail_reservation_limit_policy
#    cucascade::memory::increase_reservation_limit_policy

# Who calls a given function, two hops out, as JSON?
flox activate -- ./rust/target/release/symhow query callers 'cucascade::data_batch::make' \
  -d cucs.srctrldb --depth 2 --json

# Fuzzy symbol search.
flox activate -- ./rust/target/release/symhow query search reslimpol -d cucs.srctrldb --kind CLASS

# Where is a file, what does it look like, what's in it?
flox activate -- ./rust/target/release/symhow query file 'memory_reservation.hpp' \
  -d cucs.srctrldb --occurrences
```

The full verb list: `callers callees bases derived overrides overridden-by
members neighbors references definition search file stats`. Run
`symhow query --help` (or `symhow --help`) for the complete flag reference.

### Graph analysis

`symhow graph <VERB>` runs whole-graph algorithms over the indexed call/usage
graph — every FUNCTION/METHOD node plus every CALL edge, with USAGE edges
(field/global accesses, carrying their source-line sites) on top, and the
shared unsolved-symbol hub node excluded. This is the layer for questions
about the *shape* of the graph rather than one symbol's neighborhood.
The verb comes **before** the flags. The examples below use the debug
binary (`cargo build`, or generated when running `cargo test`).

```sh
# Which functions are the orchestrators / decision hubs?
# (Brandes betweenness; raw dependency sums — divide by
# stats.normalized_denominator = (n-1)(n-2) for a 0-1 value)
flox activate -- ./rust/target/debug/symhow graph centrality \
  -d rust/target/tmp/symhow-fixtures/fixture.srctrldb --top 10 --json

# Which load-bearing logic is reachable from the entry points?
# (personalized PageRank; --seed is repeatable, default seeds are the
# `main` functions then root-file functions)
flox activate -- ./rust/target/debug/symhow graph pagerank \
  -d rust/target/tmp/symhow-fixtures/fixture.srctrldb --seed runChain --top 10

# What is the execution backbone from an entry point — which functions
# run on EVERY path to a node? (call-graph dominator tree; --entry
# defaults to the `main` heuristic — if several `main` candidates exist
# it lists them and exits 2, pick one with --entry '#<id>')
flox activate -- ./rust/target/debug/symhow graph dominators \
  -d rust/target/tmp/symhow-fixtures/fixture.srctrldb --entry '#206' --dot /tmp/dominators.dot

# What does this function depend on — everything that can affect it?
# (backward slice over USAGE+CALL edges, function granularity;
# --lines annotates each edge with its source-line sites)
flox activate -- ./rust/target/debug/symhow graph slice \
  -d rust/target/tmp/symhow-fixtures/fixture.srctrldb --criterion level2 --lines

# What's the happy path from A to B? (weighted shortest path;
# --weight hops|calls|lines, default hops)
flox activate -- ./rust/target/debug/symhow graph path \
  -d rust/target/tmp/symhow-fixtures/fixture.srctrldb --from runChain --to level0 --weight calls
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

The parser/renderer lives in `rust/src/notes/` and provides a DB-free lint:

```sh
flox activate -- ./rust/target/release/symhow notes lint --notes-dir notes/ --strict --json
```

Phase 2 (this codebase) adds the DB-backed cache and read paths: a
`symhow_note*` cache schema in the `.srctrldb` (rebuilt from the markdown, never
the source of truth itself), matched to symbols by qualified name +
signature, with automatic staleness tracking (a hash of each symbol's
current definition body) and an overlay so querying never has to write the
DB even when a note file changed on disk since the last sync:

```sh
flox activate -- ./rust/target/release/symhow notes sync -d symhow.srctrldb --notes-dir notes/ --json
flox activate -- ./rust/target/release/symhow notes show -d symhow.srctrldb some::qualified::name --section what
flox activate -- ./rust/target/release/symhow notes status -d symhow.srctrldb --stale --json
flox activate -- ./rust/target/release/symhow notes symbols -d symhow.srctrldb --json   # feeds rust/scripts/symhow-notes-fake.py --symbols-json
```

`rust/scripts/symhow-notes-fake.py` generates deterministic fake notes trees, either
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
flox activate -- ./rust/target/release/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ --all --json
flox activate -- ./rust/target/release/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ --class my::Widget
flox activate -- ./rust/target/release/symhow notes scaffold -d symhow.srctrldb --notes-dir notes/ some::qualified::function
flox activate -- ./rust/target/release/symhow notes accept -d symhow.srctrldb some::qualified::function --json
flox activate -- ./rust/target/release/symhow notes fmt --notes-dir notes/ --check
flox activate -- ./rust/target/release/symhow index -c compile_commands.json -o symhow.srctrldb --notes-dir notes/ --force
flox activate -- ./rust/target/release/symhow update -d symhow.srctrldb --json   # re-indexes, then re-syncs notes and reports newly-stale
```

The realistic demo/test path for a fresh notes tree is `scaffold --all` (real
symbols, real hashes) → `python3 rust/scripts/symhow-notes-fake.py --fill-existing
notes/ --seed N` (fills `_TODO_` placeholders in place, preserving
headings/signatures/hashes byte-for-byte) → `sync`.

Phase 4 (final, this codebase) embeds notes directly into the call graph and
proves they travel with git, independent of any one `.srctrldb`:

```sh
flox activate -- ./rust/target/release/symhow query context 'my::qualified::function' -d symhow.srctrldb --json
flox activate -- ./rust/target/release/symhow query context 'my::qualified::function' -d symhow.srctrldb --notes-full   # text mode, full note body under each node
flox activate -- ./rust/target/release/symhow query context 'my::qualified::function' -d symhow.srctrldb --no-notes      # omit the "notes" field entirely
flox activate -- ./rust/target/release/symhow dbdiff a.srctrldb b.srctrldb --only note                                # compare ONLY the notes cache
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
category -- used by the `notes_cross_worktree` scenario test (run with
`flox activate -- bash -c 'cd rust && cargo test --test scenarios notes_cross_worktree'`)
to prove that two independently-`symhow index`d clones of
the same git history end up with an identical notes cache. New databases also
use root-relative source paths and path-derived identities; compilation-option
fingerprints and external paths can still differ across machines.

## Layout

- `rust/src/` — the Rust client, organized by module: `cli`, `common`, `db`, `graph`, `index`, `model`, `notes`, `out`, `query`, `update`.
- `rust/build.rs`, `rust/ffi/` — build wiring and the C ABI shim for the vendored writer.
- `rust/third_party/sourcetraildb/` — vendored (Apache-2.0) SourcetrailDB core writer library. See `PROVENANCE.md` there.
- `rust/scripts/symhow-notes-fake.py` — deterministic fake `symhow notes` markdown generator (see `docs/NOTES_DESIGN.md`).
- `rust/tests/` — Rust integration tests, automatic fixture support, and scenario tests.
- `rust/tests/fixtures/{synthetic,cuda}/` — hermetic source corpora; the synthetic fixture uses CMake only to generate its compilation database.
- `rust/tests/scenarios/` — update, cross-worktree notes, portable-path, and real-corpus scenario scripts, driven by `rust/tests/scenarios.rs`.
- `rust/tests/data/cucs_cdb_samples.json` — compile-command samples for argument-sanitizer tests.
- `docs/JSON_SCHEMA.md` — the `--json` output contract for every `symhow index`/`symhow query`/`symhow notes`/`symhow graph` invocation.
- `docs/KNOWN_LIMITATIONS.md` — known gaps, edge cases, and implementation notes, organized by phase.
- `docs/NOTES_DESIGN.md` — the `symhow notes` design (markdown format, cache schema, CLI, phases).
- `docs/GRAPH_SCHEMA_NOTES.md` — the `.srctrldb` schema study behind `symhow graph` (extraction SQL, per-algorithm feasibility, fixture ground truth).

## Testing

The default suite runs through Cargo; no external corpus or CUDA toolkit is
required:

```sh
flox activate -- bash -c 'cd rust && cargo test'
```

Fixture databases are generated automatically on first use by
`rust/tests/support/fixtures.rs`: CMake configures the synthetic fixture's
`compile_commands.json`, then the crate's own `symhow` binary indexes it.
Generated artifacts live under `rust/target/tmp/symhow-fixtures/`:

- `fixture.srctrldb`
- `fixture_build/compile_commands.json`
- `cuda_fixture.srctrldb`

`SYMHOW_FIXTURE_DB`, `SYMHOW_FIXTURE_CDB`, and `SYMHOW_CUDA_FIXTURE_DB`
overrides are still honored. The fixture setup needs cmake, ninja, and
libclang from the flox environment; no separate client build is required.
The `update_scenario`, `notes_cross_worktree`, and `portable_paths_cli`
scenario tests in `rust/tests/scenarios.rs` run by default.

**Known baseline issue:** nine `graph_analysis`/`graph_cli` count assertions
fail with the libclang 21 fixture shape; this was reproduced on the original
baseline and is not a client-removal regression.

Real-corpus tests are `#[ignore]` and exercise a real ~200-translation-unit
C++/CUDA project: `cucs_integration` (`rust/tests/cucs_integration.rs`) and
`cucs_update_integration` (`rust/tests/scenarios.rs`). They require
`SYMHOW_CUCS_DIR`, which the flox environment sets. The update scenario uses
a throwaway clone — never the real checkout — and compares its result with
a fresh re-index via `symhow dbdiff`:

```sh
flox activate -- bash -c 'cd rust && cargo test --release -- --ignored'
```

Run one scenario directly, or opt into only the real-corpus update test:

```sh
flox activate -- bash -c 'cd rust && cargo test --test scenarios update_scenario'
flox activate -- bash -c 'cd rust && cargo test --release --test scenarios cucs_update_integration -- --ignored'
```

`cucs_update_integration` defaults to `HEAD~1..HEAD`; override the start
revision with `SYMHOW_CUCS_UPDATE_FROM`:

```sh
SYMHOW_CUCS_UPDATE_FROM=HEAD~5 flox activate -- bash -c 'cd rust && cargo test --release --test scenarios cucs_update_integration -- --ignored'
```

The real-corpus update scenario takes roughly four minutes (three full
indexes plus the update) and prints a timing/equivalence summary at the end.

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

### Portable-path regression

The real-CLI portability regression is part of the default scenario suite,
and can also be run on its own:

```sh
flox activate -- bash -c 'cd rust && cargo test --test scenarios portable_paths_cli'
```

It indexes a temporary project, moves the checkout (removing the original),
rebinds it twice, checks queries and notes, and verifies that an incremental
header update agrees with a fresh index.

## License

symhow's own code is licensed under the Apache License, Version 2.0 (see
`LICENSE`). The vendored SourcetrailDB core writer library
(`rust/third_party/sourcetraildb/`) is also Apache-2.0, unmodified from upstream.
symhow's own indexing and query-engine logic is an original, clean-room
implementation, informed by (but not derived from) the externally observable
behavior of the Sourcetrail application (which is itself GPLv3-licensed) —
no GPLv3 source was viewed, copied, or otherwise used. See `NOTICE` for the
full third-party attributions and clean-room statement.
