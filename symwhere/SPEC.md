# symwhere — design spec & module contracts

`symwhere` is a CLI that indexes symbols (classes, functions, methods, fields, …)
from C/C++/CUDA/Rust repos using tree-sitter, stores them in SQLite, and answers
fuzzy queries. It supports full indexing, incremental updates (hash scan or git
diff), and stateless single-file parsing.

## Key facts

- CUDA uses the C++ grammar (same as helix: `.cu`/`.cuh` are cpp file-types).
- Line numbers are 1-based in output; columns 0-based (bytes).
- DB default location: `<repo>/.symwhere/index.db`.
- Repo-relative paths with `/` separators are stored.
- All builds/tests run via: `flox activate -- bash -c 'cargo ...'` from the
  `symwhere/` directory.

## Module contracts (src/)

### lang.rs — language registry (owner: queries agent)
- `pub struct LangRegistry` providing:
  - `pub fn new() -> Self`
  - `pub fn lang_by_name(name: &str) -> Option<Lang>` (uses Lang::from_str)
  - `pub fn lang_for_path(path: &Path, forced: Option<Lang>) -> Option<Lang>`
    (Lang::detect, plus override)
  - `pub fn grammar(lang: Lang) -> &'static dyn tree_sitter::Language` — wait,
    tree_sitter::Language is not dyn-safe; return `tree_sitter::Language` by value.
  - `pub fn symbols_query(lang: Lang) -> Result<tree_sitter::Query>` — compiled
    from `src/queries/<lang>/symbols.scm` via `include_str!`.
  - `pub fn capture_kind(name: &str) -> Option<SymbolKind>` — maps capture
    names like `symbol.class`, `symbol.function` … to SymbolKind.
- Grammar mapping: C→tree_sitter_c, Cpp→tree_sitter_cpp, Cuda→tree_sitter_cpp,
  Rust→tree_sitter_rust.

### queries/<lang>/symbols.scm (owner: queries agent)
- One query per language: c, cpp, rust (cuda reuses cpp).
- Capture convention: `@symbol.<kind>` for the *name node* of each definition,
  e.g. `@symbol.function`, `@symbol.method`, `@symbol.class`, `@symbol.struct`,
  `@symbol.field`, `@symbol.enum`, `@symbol.enum-member`, `@symbol.namespace`,
  `@symbol.type-alias`, `@symbol.macro`, `@symbol.constant`, `@symbol.trait`,
  `@symbol.impl`, `@symbol.module`, `@symbol.interface`, `@symbol.union`,
  `@symbol.constructor`, `@symbol.operator`, `@symbol.static`, `@symbol.variable`.
- Additionally capture `@scope` on enclosing scope nodes (namespace/class/impl
  blocks) so the indexer can build `container` (qualified scope path).
  Use `(#set! ...)` predicates only if needed; keep queries robust (avoid
  failing on minor grammar version drift).
- Study helix's `runtime/queries/{c,cpp,rust}/{locals,highlights}.scm` at
  a Helix checkout's `runtime/queries/{c,cpp,rust}` files for node names.

### walk.rs + index.rs — parallel indexer (owner: indexer agent)
- `walk.rs`:
  - `pub struct Filter { pub excludes: Vec<String>, pub includes: Vec<String>, pub langs: BTreeSet<Lang> }`
  - `pub fn walk(root: &Path, filter: &Filter) -> Vec<PathBuf>` — walkdir,
    skip hidden + common ignore dirs (`.git`, `node_modules`, `target`,
    `.symwhere`, `build`, `third_party` only when excluded via globs — no
    hardcoding beyond the list above), glob matching via a tiny glob helper
    (`fn glob_match(pattern: &str, rel_path: &str) -> bool`, support `*`, `**`,
    `?`; no external crate — hand-rolled or use existing dep already in Cargo.toml).
    Includes apply as: if non-empty, a path must match at least one include.
- `index.rs`:
  - `pub struct IndexOptions { path, langs, excludes, includes, force, db, jobs }`
  - `pub fn run_index(opts) -> Result<()>` — build/rebuild the DB:
    - resolve db path (see below), create/replace schema (`force` deletes first)
    - hash each file (blake3), parse in parallel with rayon (threads = jobs),
      extract SymbolRecords via lang registry queries + container tracking
      (`@scope`), write to SQLite in batches from a single writer thread
      (channel or collect-then-insert).
    - store meta: root path, HEAD commit if `git rev-parse HEAD` succeeds,
      schema version.
    - print a short human summary (files indexed, symbols found, elapsed).
  - `pub fn parse_file(path, lang, src: &[u8], registry) -> Result<Vec<SymbolRecord>>`
    — stateless parse used by index and `single` mode; tracks ancestor `@scope`
    captures to build `container` strings (e.g. `ns::Class` or
    `impl<T> Foo<T>` → `Foo<T>`; keep it simple: join scope names with `::`).

### store.rs + update.rs — SQLite persistence & incremental (owner: storage agent)
- `store.rs`:
  - `pub struct Store { conn: rusqlite::Connection }`
  - `pub fn db_path(root: &Path, cli_db: &Option<PathBuf>) -> PathBuf`
  - `pub fn open(path, create: bool) -> Result<Store>` (WAL mode, foreign_keys on)
  - Schema:
    ```sql
    meta(key TEXT PRIMARY KEY, value TEXT);
    files(id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, lang TEXT NOT NULL,
          hash TEXT NOT NULL, size INTEGER NOT NULL, indexed_at INTEGER NOT NULL);
    symbols(id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name TEXT NOT NULL, kind TEXT NOT NULL, container TEXT,
            start_line INTEGER NOT NULL, start_col INTEGER NOT NULL,
            end_line INTEGER NOT NULL, end_col INTEGER NOT NULL, signature TEXT);
    CREATE INDEX symbols_name ON symbols(name);
    CREATE INDEX symbols_file ON symbols(file_id);
    ```
  - Methods: `upsert_file(&FileRecord) -> Result<i64>`, `replace_symbols(file_id, &[SymbolRecord])`,
    `remove_file(path)`, `file_hashes() -> Result<HashMap<String,String>>`,
    `get_meta/set_meta`, `count_symbols()`.
- `update.rs`:
  - `pub struct UpdateOptions { path, git_diff: Option<String>, dry_run, langs, excludes, includes, db, jobs }`
  - `pub fn run_update(opts) -> Result<()>`
    - Detect changed set: if `git_diff` is Some, run `git -C root diff --name-only [--diff-filter=ACMR] [ref|HEAD-if-LAST]`
      plus `git diff --cached --name-only` when ref is LAST; else compare
      current hashes to `file_hashes()` (scan; also detect deletions and
      new files). Deleted files: remove from DB. Changed: re-parse + replace.
    - `dry_run`: print added/modified/deleted files and their symbol deltas
      without writing.
    - update `last_commit` meta after a successful non-dry git-diff update.
- Shell out to `git` via std::process::Command (no git2 crate).

### fuzzy.rs + single.rs + output.rs — query & presentation (owner: search agent)
- `fuzzy.rs`:
  - `pub struct FindOptions { queries, kinds, langs, excludes, includes, exact, limit, db }`
  - `pub fn run_find(opts) -> Result<Vec<SymbolHit>>` — load all rows
    (join files), apply hard filters (kind/lang/path globs), then fuzzy rank
    with nucleo-matcher (pattern per query, best-of over queries; case
    insensitive smartcase). When `queries` empty: return all (after filters,
    capped). `exact` → case-insensitive substring.
- `single.rs`:
  - `pub struct SymbolsOptions { file, kinds, within: Option<String>, lang, json }`
  - `pub fn run_symbols(opts) -> Result<Vec<SymbolRecord>>` — parse one file
    (uses lang registry + index::parse_file), apply kind filter; `within` =
    "N" or "A-B": keep symbols whose span encloses the line/range, or if none
    enclose exactly, return all (documented behavior: return enclosing symbols
    ordered by span size, smallest first).
- `output.rs`:
  - `pub fn emit(hits: &[SymbolHit], json: bool)` — human: `path:line:col  kind  name  [container]  (signature)`;
    JSON: array of objects with all fields.
  - `pub fn emit_symbols(syms: &[SymbolRecord], json: bool)`.
  - `pub fn print_langs()` — languages + supported kinds table.

## Error handling & style
- anyhow + context; no unwrap in library code; rustfmt; clippy clean.
- Output must be stable and machine-grep-friendly.
