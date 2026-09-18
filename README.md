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

A separate, `"integration"`-labeled test exercises the full pipeline against
a real ~200-translation-unit C++/CUDA corpus
(`/home/aaramoon/workspace/oss/cucs` by default, overridable via the
`KG_CUCS_DIR` CMake cache variable). It's excluded from the default run
above and skips itself (CTest `SKIPPED`, not `FAILED`) on any machine that
doesn't have that corpus checked out:

```sh
flox activate -- ctest --test-dir build --output-on-failure -L integration
```

## License

kg's own code is licensed under the Apache License, Version 2.0 (see
`LICENSE`). The vendored SourcetrailDB core writer library
(`third_party/sourcetraildb/`) is also Apache-2.0, unmodified from upstream.
kg's own indexing and query-engine logic is an original, clean-room
implementation, informed by (but not derived from) the externally observable
behavior of the Sourcetrail application (which is itself GPLv3-licensed) —
no GPLv3 source was viewed, copied, or otherwise used. See `NOTICE` for the
full third-party attributions and clean-room statement.
