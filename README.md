# kg

`kg` is a C++ CLI tool that indexes C++/CUDA code (via libclang) into a
Sourcetrail-schema-compatible SQLite database (`.srctrldb`), and queries
that database — call graphs, class hierarchies, overrides, references,
definitions, fuzzy symbol/file search, and corpus-wide stats — as either
human-readable text or machine-readable JSON.

**Status: feature-complete (all 6 phases done).** `kg index` is a real
libclang-backed indexer (including CUDA support, with a rung-retry ladder
for `.cu` files), and `kg query <verb>` is a full read-only query engine
with both text and `--json` output for every verb. See
`docs/KNOWN_LIMITATIONS.md` for known gaps and edge cases, and
`docs/JSON_SCHEMA.md` for the JSON output contract.

## Requirements

kg uses [flox](https://flox.dev) to provide a pinned, reproducible
toolchain — gcc, cmake, ninja, libclang, sqlite, nlohmann_json, and Catch2
all come from the project's flox environment (`.flox/`), so no system-wide
install of any of these is required (or expected to be used). Every command
below assumes you're running it through `flox activate` (or inside a shell
already `flox activate`d) from the repository root.

## Building

```sh
cd kg
flox activate -- bash -c '
  cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug
  cmake --build build -j
'
```

Or, using the checked-in CMake presets:

```sh
flox activate -- bash -c 'cmake --preset dev && cmake --build --preset dev'
```

A `release` preset (optimized, tests off) is also available:
`cmake --preset release && cmake --build --preset release`.

The resulting `kg` binary lands at `build/kg` (or `build-release/kg` for the
release preset).

```sh
flox activate -- ./build/kg --version
flox activate -- ./build/kg --help
```

## Usage tour

### Indexing

Point `kg index` at any `compile_commands.json` (the standard clang
compilation database format — CMake produces one automatically with
`CMAKE_EXPORT_COMPILE_COMMANDS=ON`, already the default for this project's
own build):

```sh
flox activate -- ./build/kg index \
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
`kg.index-summary/1` JSON object to stdout — see `docs/JSON_SCHEMA.md`),
and `--dry-run` (print each TU's fully-sanitized clang argv and exit,
without actually indexing — useful for debugging a compile-command that
won't parse).

`-j N`/`--parallel N` extracts N TUs concurrently (0 = use every CPU;
default is 1, fully sequential). The output is byte-for-byte
**deterministic regardless of N** — node ids, `kg_tu` rows, and progress
lines all come out identical to a sequential run, because a single
reorder-buffered consumer thread still feeds the one SQLite writer in
strict `compile_commands.json` order; only wall-clock time changes. Peak
memory scales roughly with N (≈ N × largest TU's AST + fact batch), so pick
N based on available RAM as well as CPU count — `-j 8` is a reasonable
default on a big multi-core box. `kg update` accepts the same `-j`/
`--parallel` flag for its re-index step.

By default (no `-q`), `kg index` prints one right-aligned `[ k/N] Ts  status
path` line per TU as it completes, plus a periodic one-line running summary
(`kg index: 120/975 done, 12.3 TU/min, elapsed 9m41s, eta ~68m`) every ~10
TUs or 5s. `-q` drops the per-TU lines but keeps that summary (now every
~60s) — the useful mode for a long unattended run; `-qq` silences
everything except the final summary line. `-v` is unaffected and still adds
its own extra detail (collision warnings, etc).

**Ctrl-C is safe.** `kg index` checkpoints every 25 consumed TUs by default
(`--commit-every N`, `0` disables checkpointing entirely and matches the
pre-checkpointing behavior of one transaction for the whole run) — a
SIGINT/SIGTERM (or a crash) loses at most the TUs since the last
checkpoint, never the whole run. An interrupted run exits 130 and prints
`kg index: interrupted after K/N TUs — completed work is saved; resume
with: kg update -d <output>`; running that `kg update` finishes exactly the
untracked remainder and produces a database logically identical to an
uninterrupted `kg index`. `kg query` still works against an in-progress
database (read-only), with a one-line stderr notice
(`index incomplete (interrupted) — run `kg update` to finish`, suppressed
by `-q`/`--json`). `kg index` itself refuses (exit 2) to overwrite an
in-progress output without `--force`, pointing at `kg update` instead.

### Updating

Once a database exists, `kg update` re-indexes only what changed since the
last `kg index`/`kg update`, instead of re-indexing the whole corpus:

```sh
flox activate -- ./build/kg update -d out.srctrldb -v
```

Everything else — the git repo root, the baseline commit, the
`compile_commands.json` path, and every `kg index` option (`--remap`,
`--include`/`--exclude`, `--cuda-mode`, ...) — is read back out of
`out.srctrldb`'s own provenance metadata (written by `kg index` at indexing
time), so a bare `kg update -d out.srctrldb` is normally enough. It diffs
the stored baseline commit against the current worktree (or `--to REV` for
two fixed commits), clears exactly the facts those changed files (and
anything that transitively `#include`s them) invalidated, and re-indexes
only the affected translation units — plus any TU whose previous run was
`partial`/`failed` (tracked per-TU in `kg_tu.status`), so a flaky parse
retries automatically without re-running the whole corpus.

**It's always the worktree that gets indexed, not a commit.** `kg update`
(no `--to`) diffs the stored baseline against `HEAD` *and* folds in the
current `git status` — so uncommitted edits are picked up and re-indexed
just like a committed change would be, and a database `kg update` produced
this way can legitimately be logically ahead of `HEAD` (it reflects
whatever is on disk right now). `kg_dirty_paths` (stored in the database)
remembers which paths were dirty at the last run, so reverting an
uncommitted edit still triggers one more re-index of that file (to bring
it back in sync with the now-clean worktree) rather than silently leaving
stale facts behind.

Useful flags: `--files PATH...` (classify these files as changed directly,
skipping git entirely — handy for scripting or when there's no repo),
`--to REV` (diff against a specific commit instead of the worktree),
`--dry-run` (show what would be cleared/re-indexed without changing
anything, no database is written), `--in-place` (modify the database
directly instead of the default temp-copy + atomic rename — faster, but an
interrupted run leaves the database unusable until re-indexed from
scratch), `--json` (a `kg.update-summary/1` object — see
`docs/JSON_SCHEMA.md`).

Exit codes: `0` success (including `--dry-run`), `1` a hard error (e.g.
can't read the compile database), `2` refused — a database with no
provenance at all (never indexed by this `kg`, or by the real Sourcetrail
indexer — such a database can't be updated, only re-indexed from scratch
with `kg index`), one written by an incompatible walker version, a
pre-Phase-4 `kg_tu` table with no `status` column, one an interrupted
`--in-place` run left half-updated, or `--system-file-rows=all` (not
supported incrementally) — and `5` nothing changed since the last run (the
database is untouched; this is not an error). See
`docs/KNOWN_LIMITATIONS.md`'s Phase 3/4 sections for the full invalidation
algorithm.

### Querying

Every verb accepts `-d/--db PATH` (default `$KG_DB` or `./kg.srctrldb`) and
`--json` (see `docs/JSON_SCHEMA.md` for the exact schema each verb emits).
A few examples, run against a database indexed from the
[cuCascade](https://github.com/nvidia/cuCascade) corpus (a GPU memory
management library — real-world C++20/CUDA code, used as kg's own
integration-test corpus; see "Testing" below):

```sh
# Corpus-wide stats: node/edge counts by kind, file counts, recorded errors.
flox activate -- ./build/kg query stats -d cucs.srctrldb

# Who derives from a base class?
flox activate -- ./build/kg query derived 'cucascade::memory::reservation_limit_policy' \
  -d cucs.srctrldb --depth 0
# -> cucascade::memory::ignore_reservation_limit_policy
#    cucascade::memory::fail_reservation_limit_policy
#    cucascade::memory::increase_reservation_limit_policy

# Who calls a given function, two hops out, as JSON?
flox activate -- ./build/kg query callers 'cucascade::data_batch::make' \
  -d cucs.srctrldb --depth 2 --json

# Fuzzy symbol search.
flox activate -- ./build/kg query search reslimpol -d cucs.srctrldb --kind CLASS

# Where is a file, what does it look like, what's in it?
flox activate -- ./build/kg query file 'memory_reservation.hpp' \
  -d cucs.srctrldb --occurrences
```

The full verb list: `callers callees bases derived overrides overridden-by
members neighbors references definition search file stats`. Run
`kg query --help` (or `kg --help`) for the complete flag reference.

### Notes

`kg notes` attaches durable, reviewable function-level knowledge (brief /
what / how / signature / pre-conditions / post-conditions / bugs /
suggestions) to symbols, stored as markdown in git so it's versioned,
diffable, and shared across worktrees/sessions. See `docs/NOTES_DESIGN.md`
for the full design and markdown format.

Phase 1 shipped the parser/renderer (`kg_notes` library, `include/kg/notes/`)
and a DB-free lint:

```sh
flox activate -- ./build/kg notes lint --notes-dir notes/ --strict --json
```

Phase 2 (this codebase) adds the DB-backed cache and read paths: a
`kg_note*` cache schema in the `.srctrldb` (rebuilt from the markdown, never
the source of truth itself), matched to symbols by qualified name +
signature, with automatic staleness tracking (a hash of each symbol's
current definition body) and an overlay so querying never has to write the
DB even when a note file changed on disk since the last sync:

```sh
flox activate -- ./build/kg notes sync -d kg.srctrldb --notes-dir notes/ --json
flox activate -- ./build/kg notes show -d kg.srctrldb some::qualified::name --section what
flox activate -- ./build/kg notes status -d kg.srctrldb --stale --json
flox activate -- ./build/kg notes symbols -d kg.srctrldb --json   # feeds scripts/kg-notes-fake.py --symbols-json
```

`scripts/kg-notes-fake.py` generates deterministic fake notes trees, either
from a bare JSON symbol list or directly from `kg notes symbols --json`'s
own output; `--stale-ratio`/`--drop-hash-ratio` inject deterministic
staleness/unanchored-note scenarios for testing.

Phase 3 (this codebase) adds writing: `kg notes scaffold` creates
placeholder blocks (idempotent and non-destructive -- existing blocks are
never rewritten), `kg notes accept` re-anchors a note's `kg-hash`/
`kg-commit` after reviewing a change, `kg notes fmt` is a plain canonical
re-render (`--check` for CI), and `kg update` re-syncs notes automatically
at the end of every run so staleness tracks the code without a separate
step:

```sh
flox activate -- ./build/kg notes scaffold -d kg.srctrldb --notes-dir notes/ --all --json
flox activate -- ./build/kg notes scaffold -d kg.srctrldb --notes-dir notes/ --class my::Widget
flox activate -- ./build/kg notes scaffold -d kg.srctrldb --notes-dir notes/ some::qualified::function
flox activate -- ./build/kg notes accept -d kg.srctrldb some::qualified::function --json
flox activate -- ./build/kg notes fmt --notes-dir notes/ --check
flox activate -- ./build/kg index -c compile_commands.json -o kg.srctrldb --notes-dir notes/ --force
flox activate -- ./build/kg update -d kg.srctrldb --json   # re-indexes, then re-syncs notes and reports newly-stale
```

The realistic demo/test path for a fresh notes tree is `scaffold --all` (real
symbols, real hashes) → `python3 scripts/kg-notes-fake.py --fill-existing
notes/ --seed N` (fills `_TODO_` placeholders in place, preserving
headings/signatures/hashes byte-for-byte) → `sync`.

Phase 4 (final, this codebase) embeds notes directly into the call graph and
proves they travel with git, independent of any one `.srctrldb`:

```sh
flox activate -- ./build/kg query context 'my::qualified::function' -d kg.srctrldb --json
flox activate -- ./build/kg query context 'my::qualified::function' -d kg.srctrldb --notes-full   # text mode, full note body under each node
flox activate -- ./build/kg query context 'my::qualified::function' -d kg.srctrldb --no-notes      # omit the "notes" field entirely
flox activate -- ./build/kg dbdiff a.srctrldb b.srctrldb --only note                                # compare ONLY the notes cache
```

`kg query context SYMBOL` returns the seed plus its callers/callees (to
`--depth`), one-hop overrides/overridden-by, and (for a CLASS/STRUCT seed)
one-hop bases/derived -- the exact same `kg.graph/1` envelope every other
graph verb uses, with one addition: every node object gets a `"notes"` field
(the full note object, or `null`), sourced from the same overlay-aware read
path `kg notes show`/`status` use (a notes file changed on disk since the
last `sync` prints the usual "notes cache out of date" hint to stderr;
`context` never writes the DB). This is the intended entry point for an
agent picking up a function cold: one call gets the code's shape AND
whatever knowledge was already written down about it and its neighbors --
see `.claude/skills/kg/SKILL.md`'s "Function notes" section for the
read-a-note / write-a-note / accept-a-stale-note workflow.

`kg dbdiff` also grew a `note`/`note_problem` category (from `kg_note`/
`kg_note_problem`, keyed by `serialized_name` -- never by `node.id` or a
file path) and a `--only CATEGORY` flag to restrict a diff to just one
category -- used by the `notes_cross_worktree` test (`ctest -R
notes_cross_worktree`) to prove that two independently-`kg index`d clones of
the same git history end up with an identical notes cache even though their
node/edge/location rows (which embed each worktree's own absolute paths)
legitimately do not.

## Layout

- `cmake/` — CMake helper modules (`FindLibClang.cmake`, `KgWarnings.cmake`, `KgFixtures.cmake`).
- `third_party/sourcetraildb/` — vendored (Apache-2.0) SourcetrailDB core writer library. See `PROVENANCE.md` there.
- `include/kg/`, `src/` — kg's own first-party code, organized by module: `common`, `model`, `index`, `db`, `query`, `out`, `cli`, `notes`.
- `scripts/kg-notes-fake.py` — deterministic fake `kg notes` markdown generator (see `docs/NOTES_DESIGN.md`).
- `test/unit/`, `test/integration/`, `test/fixtures/` — Catch2-based tests (see "Testing" below).
- `docs/JSON_SCHEMA.md` — the `--json` output contract for every `kg index`/`kg query`/`kg notes` invocation.
- `docs/KNOWN_LIMITATIONS.md` — known gaps, edge cases, and implementation notes, organized by phase.
- `docs/NOTES_DESIGN.md` — the `kg notes` design (markdown format, cache schema, CLI, phases).

## Testing

The default test suite (89 original + several Phase 6 additions, all
hermetic — no external corpus or CUDA toolkit required) runs via:

```sh
flox activate -- ctest --test-dir build --output-on-failure -LE integration
```

A separate, `"integration"`-labeled pair of tests exercises the full
pipeline against a real ~200-translation-unit C++/CUDA corpus
(`/home/aaramoon/workspace/oss/cucs` by default, overridable via the
`KG_CUCS_DIR` CMake cache variable): `cucs_integration` (a fresh `kg index`
of the whole corpus) and `cucs_update_integration` (`kg update` over a real
commit range in that same corpus, inside a throwaway clone — never the
real checkout — verified against a fresh re-index via `kg dbdiff`). Both are
excluded from the default run above and skip themselves (CTest `SKIPPED`,
not `FAILED`) on any machine that doesn't have the corpus (or `git`/
`sqlite3`) available:

```sh
flox activate -- ctest --test-dir build --output-on-failure -L integration
```

`cucs_update_integration` is also labeled `"update"`, alongside the
synthetic-fixture `update_scenario` test, so `ctest -L update` runs both:

```sh
flox activate -- ctest --test-dir build --output-on-failure -L update
```

`cucs_update_integration` defaults to the commit range `HEAD~1..HEAD` in
the cucs corpus; override with `KG_CUCS_UPDATE_FROM` (read directly by the
test script at run time, so no reconfigure is needed) for a bigger range:

```sh
KG_CUCS_UPDATE_FROM=HEAD~5 flox activate -- ctest --test-dir build --output-on-failure -R cucs_update_integration
```

Both real-corpus update runs take roughly 4 minutes (three full indexes of
the corpus plus the update itself) and print a timing/equivalence summary
at the end.

What `cucs_update_integration` asserts about equivalence, in order:

1. Every garbage-collection predicate `kg update` deletes by matches **0
   rows on the freshly indexed HEAD database** — the rules are proven
   non-destructive against a full `kg index`, not just assumed to be.
2. A strict, no-exclusions `kg dbdiff` of the updated database against the
   fresh one: it may **never** lose a row a fresh index has, and any
   surviving difference must be an occurrence `location` (never a node,
   edge, symbol, file or TU row).
3. `kg dbdiff --ignore shared-header-locations`, which must be exactly
   empty. That flag drops occurrence locations inside multiply-included
   headers, and nothing else; it exists for one inherent limitation —
   template-instantiation facts land in a shared header's text but belong
   to the TU that instantiated them, and kg's fact provenance is per-file,
   not per-TU. See `docs/KNOWN_LIMITATIONS.md`'s Phase 5 section for the
   measurements behind this.
4. The single-file speed run's `kg dbdiff` is checked with **no**
   `--ignore` at all and must be exactly equivalent.

## License

kg's own code is licensed under the Apache License, Version 2.0 (see
`LICENSE`). The vendored SourcetrailDB core writer library
(`third_party/sourcetraildb/`) is also Apache-2.0, unmodified from upstream.
kg's own indexing and query-engine logic is an original, clean-room
implementation, informed by (but not derived from) the externally observable
behavior of the Sourcetrail application (which is itself GPLv3-licensed) —
no GPLv3 source was viewed, copied, or otherwise used. See `NOTICE` for the
full third-party attributions and clean-room statement.
