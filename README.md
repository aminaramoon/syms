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
from), `--include`/`--exclude GLOB` (filter which TUs get processed),
`--cuda-mode auto|full|nowrapper|host-only|skip` (how to handle `.cu`
files — `auto`, the default, retries a `.cu` TU across progressively more
permissive rungs until one succeeds), `--json` (also print a
`kg.index-summary/1` JSON object to stdout — see `docs/JSON_SCHEMA.md`),
and `--dry-run` (print each TU's fully-sanitized clang argv and exit,
without actually indexing — useful for debugging a compile-command that
won't parse).

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

## Layout

- `cmake/` — CMake helper modules (`FindLibClang.cmake`, `KgWarnings.cmake`, `KgFixtures.cmake`).
- `third_party/sourcetraildb/` — vendored (Apache-2.0) SourcetrailDB core writer library. See `PROVENANCE.md` there.
- `include/kg/`, `src/` — kg's own first-party code, organized by module: `common`, `model`, `index`, `db`, `query`, `out`, `cli`.
- `test/unit/`, `test/integration/`, `test/fixtures/` — Catch2-based tests (see "Testing" below).
- `docs/JSON_SCHEMA.md` — the `--json` output contract for every `kg index`/`kg query` invocation.
- `docs/KNOWN_LIMITATIONS.md` — known gaps, edge cases, and implementation notes, organized by phase.

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
