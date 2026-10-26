---
name: symwhere
description: >
  Fast fuzzy symbol search and code-structure queries for C/C++/CUDA/Rust
  repositories. Use when you need to find where classes, functions, methods,
  fields, or other symbols are defined across a repo, determine which
  function/class encloses a given line, get an outline of a single file, or
  incrementally refresh a repo-wide symbol index after code changes. Faster
  and more precise than grep for "where is this symbol defined" questions;
  requires a one-time `symwhere index` per repo.
---

# symwhere — symbol index for code navigation

`symwhere` indexes **definitions** (not references) of symbols with
tree-sitter and stores them in SQLite. Binary lives at
`target/release/symwhere` (build with
`flox activate -- bash -c 'cargo build --release'` from the repo root).

Supported languages: `c`, `cpp`, `cuda` (uses the C++ grammar), `rust`.
Supported kinds: `class struct enum union interface trait impl function method
constructor field constant static variable type-alias namespace module macro
enum-member operator`.

## First use per repo (one time)

```sh
symwhere index /path/to/repo            # parallel; db at <repo>/.symwhere/index.db
symwhere index . -e third_party -e 'src/gen/**' -j 8   # with excludes
```

Read-only commands (`find`, `symbols`) then work from **any directory inside
the repo** — the db is discovered by walking up, git-style; `--db PATH`
overrides.

## Finding symbols (replaces multi-grep sweeps)

```sh
symwhere find ringbuf                          # fuzzy: matches RingBuffer, ring_buffer_write...
symwhere find push -t method -l cpp            # filter by kind and language
symwhere find "buffer" -e third_party -n 100   # path excludes, more results
symwhere find Mixer --json                     # machine-readable (one JSON object per line)
symwhere find "parse_" --exact                 # case-insensitive substring instead of fuzzy
symwhere find push --spans                     # add the end of the definition
```

`--spans` extends the location with the definition's end position — ideal when
you need to read the *entire* definition, not just jump to its name:
`src/ring_buffer.cpp:8:20-13:1` means the function body runs from line 8 to
line 13.

Human output format (grep-friendly):

```
src/ring_buffer.cpp:8:20  method  push  [dsp::RingBuffer]  void RingBuffer<T>::push(const T& value)
```

`[container]` is the qualified enclosing scope (`ns::Class`), the trailing
part is a one-line signature. With `--spans` the location becomes
`path:line:col-endline:endcol`; JSON output always carries both ends. JSON fields: `name, kind, container, path, lang,
start_line, start_col, end_line, end_col, signature` (1-based lines, 0-based
byte columns; `signature`/`container` may be `null`).

## Enclosing-scope questions ("what contains line N?")

```sh
symwhere symbols src/foo.cpp --within 342          # symbols enclosing line 342
symwhere symbols src/foo.cpp --within 120-180      # ...or a range
symwhere symbols src/foo.cpp --within 342 -t method
symwhere symbols src/foo.cpp                       # full file outline (no db needed at all)
symwhere symbols src/foo.cpp --spans               # outline with end positions
```
To read a definition body end-to-end: grep `--spans` output, then read
`path` from `start_line` through `end_line`.

`--within` prints the smallest enclosing span first (innermost→outermost), so
the first line answers "which function am I in?" and later lines give the
class/namespace chain. Single-file mode never touches the database — safe in
any checkout, no index required.

## Keeping the index fresh after edits

```sh
symwhere update . --git-diff --dry-run   # preview: A/M/D files with symbol deltas
symwhere update . --git-diff             # diff vs the commit stored at last index time
symwhere update . --git-diff HEAD~3      # diff vs an explicit ref
symwhere update .                        # no git? hash-scan everything (blake3)
```

`--git-diff` unions staged, unstaged, and (for `LAST`) untracked files, then
hash-verifies each candidate, so over-inclusion is harmless. Dry-run output:
`M src/lib.rs (+5 -0 symbols)`, `A new.rs (+12)`, `D old.rs (-7)`.

## Gotchas (learned the hard way)

- Definitions only: `find` will not show call sites or references — use grep
  for those; symwhere tells you *where things are defined*.
- Non-templated C++ out-of-class members (`Foo::bar()`) are typed as `method`
  via a CamelCase heuristic (the CST is identical to `ns::fn()`); an uppercase
  *namespace* may be misread as a class. Templated members (`RingBuffer<T>::push`)
  are structurally unambiguous.
- C file-scope `static`/local variables are not indexed yet; C captures are
  functions, macros, aggregates, fields, enum members, typedefs.
- Empty result ≠ error (exit 0). Errors print `error: ...` and exit 1.
- Piping `--json` output into `head` can SIGPIPE the tool (exit 1 despite
  valid output) — use `-n` to limit instead of `head`.
- Index db is per-repo state at `<repo>/.symwhere/index.db`; it is disposable —
  `index --force` rebuilds from scratch in well under a second for ~250-file
  repos (10k+ symbols).
- Query files (`src/queries/*/symbols.scm`) encode grammar-version-specific
  node shapes; validate any edit by probing the CST, not by eyeballing.
