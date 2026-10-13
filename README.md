# symwhere

Fast, fuzzy, incremental symbol index for C / C++ / CUDA / Rust repositories,
powered by tree-sitter.

> AI agents: see [SKILL.md](SKILL.md) for a capability cookbook, and
> [AGENTS.md](AGENTS.md) for repo-level working conventions.

## Commands

```sh
symwhere index [PATH]             # build the symbol index (parallel, tree-sitter)
  -l, --lang <c|cpp|cuda|rust>    # restrict languages
  -e, --exclude <GLOB>...         # exclude paths (e.g. third_party, src/gen/**)
  -i, --include <GLOB>...         # only include paths
  -f, --force                     # rebuild from scratch
  -d, --db <PATH>                 # db location (default <repo>/.symwhere/index.db)
  -j, --jobs <N>                  # worker threads

symwhere update [PATH]            # incremental update
  --git-diff [REF]                # changed files via git diff (default: vs last indexed commit)
  --dry-run                       # show what would change, don't write

symwhere find [QUERIES]...        # fuzzy search the index
  -t, --type <class,method,...>   # filter by symbol kind
  -l, --lang, -e, -i              # same filters as index
      --exact                     # substring instead of fuzzy
  -n, --limit                     # max results (default 50)
      --spans                     # add end position: path:line:col-el:ec
      --json                      # machine-readable output (always has spans)

symwhere symbols <FILE>           # symbols in one file, no database
  -t, --type <KINDS>              # kind filter
      --within <LINE|RANGE>       # symbols enclosing a line or range (innermost first)
  -l, --lang <LANG>               # override detection
      --spans                     # add end position
      --json

symwhere langs                    # supported languages & kinds
```

## Examples

```sh
symwhere index .
symwhere find ringbuf --type class,struct
symwhere find "push" -l cpp -e third_party
symwhere update . --git-diff           # since last indexed commit
symwhere update . --git-diff HEAD~3
symwhere update . --git-diff --dry-run # patch preview
symwhere symbols src/ring_buffer.cpp
symwhere symbols include/ring_buffer.hpp --within 10-40 --type method
```

## Design

- Parsing: tree-sitter grammars (`tree-sitter-c`, `-cpp`, `-rust`); CUDA
  (`.cu`/`.cuh`) uses the C++ grammar, like Helix.
- Queries: one `symbols.scm` per language capturing `@symbol.<kind>` and
  `@scope`, modeled on Helix's query conventions.
- Storage: SQLite (WAL) — files + symbols tables, keyed by blake3 content hash
  for cheap incremental updates.
- Fuzzy search: nucleo-matcher (the matcher Helix uses).
- Parallelism: rayon across files; single SQLite writer.
