# AGENTS.md — guidance for AI agents working on symwhere

`symwhere` is a Rust CLI that indexes code symbols (classes, functions,
methods, fields, …) with tree-sitter, stores them in SQLite, and answers fuzzy
queries. It supports full indexing, incremental updates (git diff or hash
scan), and stateless single-file parsing.

## Layout

| Path | What it is |
|---|---|
| `src/` | The crate: CLI, tree-sitter pipeline, storage, search (see SPEC.md) |
| `src/queries/{c,cpp,rust}/symbols.scm` | Per-language symbol queries (cuda reuses cpp) |
| `fixtures/demo/` | Multi-language fixture (C, C++, CUDA, Rust, git history) for e2e tests |
| `SPEC.md` | Module contracts — read before changing any module API |
| `SKILL.md` | Capability cookbook for agents *using* the tool |
| `README.md` | User-facing docs |

## Building, testing, linting

All dependencies are managed by **flox** (manifest in the parent workspace's
`.flox/`, or run `flox use` from this directory if standalone). Always run
cargo through the flox env:

```sh
flox activate -- bash -c 'cargo build'
flox activate -- bash -c 'cargo test'        # 55 tests + 1 ignored probe
flox activate -- bash -c 'cargo fmt'         # run before finishing
```

Known quirk: `flox activate -- bash -c 'cargo clippy'` resolves a rustup shim
and fails. Run clippy via `~/.cargo/bin/cargo clippy --all-targets` instead.
Do **not** run plain `bash -lc` under flox activate — a login shell resets
PATH and loses the flox env.

Current expected state: `cargo test` → 55 passed / 0 failed / 1 ignored;
`cargo clippy --all-targets` → 0 warnings; `cargo fmt --check` → clean.

## Architecture map

- `src/main.rs` / `src/cli.rs` — clap-derive CLI + dispatch (4 subcommands).
- `src/types.rs`, `src/kinds.rs` — `Lang`, `SymbolKind`, `SymbolRecord`,
  `SymbolHit` + CLI-arg conversions. Lines are 1-based, columns 0-based (bytes).
- `src/lang.rs` — `LangRegistry`: grammar per language + compiled
  `symbols.scm` queries + capture-name → kind mapping. CUDA uses the C++
  grammar (same as Helix).
- `src/queries/*/symbols.scm` — capture `@symbol.<kind>` on name nodes and
  `@scope` on scope names. Tuned against crates.io grammar versions
  (c/cpp 0.23.4, rust 0.24.2): probe the CST before changing patterns
  (a `/tmp/tsprobe`-style scratch harness is the established method).
- `src/walk.rs` — filesystem walk + hand-rolled glob matching
  (`*`, `**`, `?`; slash-less patterns match any path segment; excludes win).
- `src/index.rs` — rayon-parallel parse pipeline, one SQLite writer thread fed
  via mpsc; `parse_file` is the stateless single-file entry point.
- `src/store.rs` — SQLite (WAL): `meta`/`files`/`symbols` schema; writes are
  atomic via `upsert_file_with_symbols`.
- `src/update.rs` — incremental updates: `--git-diff [REF|LAST]` (unions
  worktree/index/untracked, `-z` safe) or blake3 hash-scan; `--dry-run` prints
  the would-be patch without writing.
- `src/fuzzy.rs` — nucleo-matcher ranking + git-style upward `--db` discovery.
- `src/single.rs`, `src/output.rs` — `--within` enclosing-symbol logic,
  `--spans` end positions, and human/JSON output
  (`path:line:col  kind  name  [container]  signature`).

## Working rules

- Read `SPEC.md` before changing module APIs; keep signatures stable — several
  modules depend on each other's exact shapes.
- Doc claims may drift: trust the code and tests first, then update docs in
  the same change.
- After any change: `cargo fmt`, full `cargo test`, clippy, and an e2e pass
  against `fixtures/demo` (recipe in `SKILL.md`).
- Never commit generated artifacts (`target/`, `*.db`, `.symwhere/`) — the
  `.gitignore` covers these.
