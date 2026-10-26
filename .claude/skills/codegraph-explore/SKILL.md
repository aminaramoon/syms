---
name: codegraph-explore
description: Explore and understand a C/C++/CUDA codebase (any repo that has — or for which you build — a symhow/symwhere index) through those indexes instead of grep — find where things are defined, read class outlines, call graphs with call sites, class hierarchies and every override of a virtual, field readers/writers, exact meaning of a line, header include impact, what a change touches, and execution paths through virtual dispatch. Use when orienting in an unfamiliar area, before modifying code (impact analysis), when looking for an existing utility to reuse, or after editing to refresh the indexes. Pairs with codegraph-debug for symptom-driven investigation.
---

# codegraph-explore — Sourcetrail-style graph + tree-sitter outline for C/C++

Two prebuilt indexes per repo, plus `symx`, a helper that does what the raw CLIs can't do cleanly.

| | **symhow** (`.srctrldb`, libclang) | **symwhere** (`symwhere.db`, tree-sitter) |
|---|---|---|
| Knows | calls, references, field reads/writes, inheritance, overrides, includes, exact overloads, full namespaces | where definitions are, with full body spans |
| Coverage | only the TUs in `compile_commands.json`, **in this build configuration** | **every file on disk**: tests, docs, Rust, other platforms, `#if`-disabled code |
| Weak at | code outside the build, inactive `#if` branches, dispatch through virtuals, fn-pointers and callbacks | anything semantic (no references or calls); `.h` needs `-l cpp` in C++ repos |
| Speed | 5–300 ms per query | 0.3 s `find` over 170k symbols; single-file outline needs no DB |

**Rule of thumb:** symhow answers *who/what is connected*. symwhere answers *where is it, and what's in this file*, including code the build never compiled.

## Setup (once per session)

```bash
# symx is the helper that ships with this skill (stdlib python only).
# Script paths below are relative to THIS skill's directory; or copy scripts/* onto your $PATH.
X=scripts/symx
H=symhow            # symhow CLI — the Rust build is a superset of the C++ one (adds `graph`)
W=symwhere          # symwhere CLI
cd <repo>; $X status        # which DBs, how fresh, known warnings — ALWAYS do this first
```

**Binaries.** `symhow` and `symwhere` must be on `$PATH` (or set `$SYMHOW` / `$SYMWHERE` to their full paths). If you don't have them, build them from this repo:
- `symhow`: `cd symhow/rust && cargo build` → `symhow/rust/target/debug/symhow` (has `graph`). A C++ indexer also builds at `symhow/cpp/build/symhow`.
- `symwhere`: `cd symwhere && cargo build` → `symwhere/target/debug/symwhere`.
- **Never wrap queries in `flox activate`** — activation can hang for minutes on a network call. Run the binaries directly.

**Databases.** `symx` needs one symhow DB (`.srctrldb`) and one symwhere DB (`symwhere.db`) per repo. It discovers them under `$SYMX_DBS` (default `~/explorer`), expecting `$SYMX_DBS/<repo>/how/<repo>.srctrldb` and `$SYMX_DBS/<repo>/where/symwhere.db`; set `$SYMX_DBS` to wherever you keep indexes, or select one with `--repo NAME|PATH`. For raw `symhow` calls pass `-d DB`; for `symwhere` pass `-d DB` or run inside the repo.
- **No index for your repo yet? Create one** — see "Creating the indexes" below.
- If `symx status` says **STALE**, results for the listed files describe the old code. Refresh them (see "After editing") or read those files directly.

## Creating the indexes (when none exist yet)

```bash
# symhow (.srctrldb): needs a compile_commands.json for the repo
#   (configure it with -DCMAKE_EXPORT_COMPILE_COMMANDS=ON to produce one).
$H index -c compile_commands.json -o <repo>.srctrldb --exclude '**/_deps/**' --exclude-submodules -j 8 --force

# symwhere (symwhere.db): indexes every file on disk, no build needed; keep tests in.
$W index <repo> --force -e '_build/**'
# C++ repos only — re-parse headers as C++ (index/update parse .h as C):
python3 scripts/symwhere-h-as-cpp.py <path/to/symwhere.db> <repo>
```

Then either move the DBs where `symx` looks (`$SYMX_DBS/<repo>/how/<repo>.srctrldb` and `$SYMX_DBS/<repo>/where/symwhere.db`), or point the raw CLIs at them with `-d`. **Toolchain traps** (nix/conda compilers, generated headers, CUDA, which TUs to exclude) are real and repo-specific — read `references/recipes.md` § Building indexes before a full run.

## Question → command

| Question | Command |
|---|---|
| Where is X defined? (fuzzy) | `$W find X -t class,struct --spans -n 10` · `$H query search 'Cls::meth' -d DB --limit 5` |
| Outline of a file / class | `$W symbols FILE -l cpp --spans` (no DB needed) · `$H query members '#ID' -d DB` |
| What does this function call? | `$X tree SYM --callees --depth 2` |
| Who calls it? | `$X tree SYM --callers --depth 3` (dead ends are annotated with what to do next) |
| What exactly is on this line? | `$X at FILE:LINE[-END]`: enclosing function, plus each call/field/type/enum resolved to its exact overload and definition |
| All implementations of a virtual | `$X overriders 'Base::method'` (transitive, includes the class-template fallback) |
| Class hierarchy | `$H query bases|derived 'ns::Cls' -d DB --depth 0` |
| Who writes / reads a field? | `$X writes 'ns::Cls::field_' [--reads]` |
| Everything referencing a symbol | `$H query references SYM -d DB --json` (use the `context` field = enclosing function) |
| Where are instances created? | `$H query references 'ns::Cls' -d DB --json` → TYPE_USAGE sites (factories); ctor callers via `--all-matches` |
| How does A reach B at runtime? | `$X reach A B` (follows virtual dispatch; `--fnptr` for C ops tables, `--usage` for callback registration) |
| What must run before X? | `$X chain X --entry E` (dominator waypoints) |
| Cost of touching a header | `$X includers path/to/header.h` (TUs that rebuild, grouped by dir) |
| What did my diff touch? | `$X changed [REV] [--tests]` (changed functions plus their callers/overriders, and with `--tests` the test cases that reach them) |
| Which tests exercise X? | `$X tests SYM` (Catch2 `TEST_CASE` names / gtest bodies reached through callers; sirius and ucx indexes include tests) |
| Load-bearing code in an area | `$H graph pagerank -d DB --seed '#ID' --top 30` (ignore `std::` entries) |
| Decode ids from raw SQL | `$X name 123 456` |

## Workflows

**Orient in an unfamiliar subsystem**
1. `$W find <concept> -t class,struct -i '<subdir>/**' --spans` to find the main types.
2. `$H query derived 'ns::Base' -d DB --depth 0` for the family tree. Then `$X overriders 'ns::Base::keyMethod'` to see how each implementation differs.
3. `$W symbols <header> -l cpp -t method,field --spans` for the API surface with line ranges. Then read only the spans you need.
4. `$H graph pagerank -d DB --seed '#<entry id>' --top 30` ranks what the entry point leans on (skip `std::`/`fmt::` rows).

**Understand one function**
1. `$H query definition SYM -d DB` gives the body span; read just that range.
2. `$X tree SYM --callees --depth 2` shows what it calls, with call-site lines.
3. For confusing lines: `$X at FILE:L1-L2` says which overload, field, or enum each token really is.
4. `$X tree SYM --callers` shows how it's used, which reveals the implicit contract.

**Before changing code (impact analysis)**
- `$X tree SYM --callers --depth 3 --all` (`--all` unions overloads and template instantiations) plus `$X overriders SYM` if it's virtual.
- Changing a signature or semantics of a type: `$H query references 'ns::Type' -d DB --json`.
- Editing a header: `$X includers hdr.h`.
- **Which tests to run:** the sirius and ucx indexes include tests, so `$X tests SYM` (or `$X changed --tests`) lists the exact test cases that reach the code. Run those first. The folly/velox indexes were built **without** tests. There, use `$W find <name> -i '**/test*/**'` or `grep -rn <name> --include=*Test*.cpp`.

**Reuse instead of reinventing:** `$W find <verb-ish name> -t function,method -n 30` and `$H query search '<ns>::<guess>' -d DB`. Check callers to see idiomatic usage.

## After editing (keep the indexes in the loop)

```bash
cd <repo>; $X changed            # what you touched + its callers (run BEFORE updating)
# symhow: incremental re-index of the touched TUs (~4–30 s each):
$H update -d <path/to/<repo>.srctrldb> -j 8
# symwhere: same excludes as the full index, then re-run the .h repair (update re-parses .h as C!):
$W update <repo> --git-diff -d <path/to/symwhere.db> -e 'build/**' -e '_build/**'
python3 scripts/symwhere-h-as-cpp.py <path/to/symwhere.db> <repo>   # C++ repos only
```

Node ids of untouched code stay stable across `symhow update`, so cached `#ID`s remain valid. The Rust `symhow` indexer can stall for many minutes on some TUs — for large C++ repos the C++ indexer (`symhow/cpp/build/symhow`) is faster; see `references/recipes.md` for the toolchain env it needs. After rebuilding a DB or editing `symx`, run `scripts/symx-selftest.sh <repo>`. To build an index for a **new** repo, see `references/recipes.md` § Building indexes — it has several non-obvious traps.

## Gotchas (all observed on these repos)

**symhow**
- Prefer `--json`, or the `symx` wrappers. The text tree of `callers/callees --depth ≥2` **indents by depth, not by parent**: it showed `Range::Range` "calling" `allocExtBuffer`. Text `references` prints bare ids, and `context` text mixes callers and callees with no direction.
- Overloads make names ambiguous (exit 2, candidates printed). Re-run with `'#ID'`. Template instantiations become separate nodes (`BlockingQueue::add(T&&)` vs `add(CPUTask&&)`), so for "all callers" use `--all-matches` / `symx --all`.
- `--kind K` filters the **returned** nodes too: `members X --kind CLASS` hides every member. Resolve the id first, then query without `--kind`.
- Constructors of class templates are named `Cls<T, …>`; find them with `search`.
- `overridden-by` is one hop (it missed `CPUThreadPoolExecutor::add`), so use `symx overriders`. **No OVERRIDE edges exist inside class templates** (`BlockingQueue<T>` → `UnboundedBlockingQueue<T>`); `symx overriders` falls back to same-named methods in derived classes.
- CALL edges point at the *static* target. A virtual call stops at the base method, and a call through a fn-pointer field is a CALL to the **FIELD** node. Lambdas passed as callbacks have no callers; their qualified name contains the enclosing function.
- "0 callers" is a hint, not proof. TU-local "dead" functions were ≥75% false positives (macros, templates, weak symbols, callbacks). Confirm with grep.
- Coverage holes: code under `#if FOLLY_HAS_LIBURING` (liburing absent) is invisible; sirius CUDA is parsed host-side by clang (`.cu` kernels and `<<<>>>` launch CALL edges are present; device-only code paths and nvcc-only features can be thin), and its build-generated headers are indexed from `dbs/sirius/generated/`; velox `_deps/`, `tests/`, and the parquet Arrow writer (Arrow absent) are excluded or degraded; ucx has 6 failed TUs. `symx at` tells you when you're in such a hole.
- Edge call-site lists can include **spurious locations on unrelated macro lines**. For velox, `incrementReservationNonThreadSafe → incrementReservationThreadSafe` listed `Buffer.h:231` (`VELOX_NYI`) among 6 sites, and only 1 was real. `symx` keeps only sites inside the caller's body. With raw JSON, do the same.
- Columns are 1-based with an exclusive end: the token is `line[start_column-1 : end_column-1]`.
- Paths are absolute and sometimes uncanonical: ucx `…/contrib/../src/…`, velox `…/velox/./velox/…`. Match by suffix (`symx` does).

**symwhere**
- `.h` maps to the **C grammar**, which drops ~60% of C++ header symbols. The dbs copies for folly/velox are repaired, but `symbols X.h` must get `-l cpp` in C++ repos (not in ucx, which is C).
- Definitions only (no references). Forward declarations show up as `class` hits; use `--spans` (a one-line span means a declaration).
- `symbols --within L` lists innermost first. **Trust the first line**: a misparse claimed `AsyncSocket::connect` spans lines 962–5062. With no enclosing symbol it dumps the whole outline.
- velox's C++17 nested namespaces (`namespace facebook::velox {`) leave ~20% of symbols with no container, so filter by path (`-i 'velox/exec/**'`) rather than by namespace.
- Don't pipe `--json` into `head` (SIGPIPE, exit 1); use `-n`.
