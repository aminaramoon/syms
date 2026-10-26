# codegraph recipes — schema, raw SQL, and building indexes

Read this when `symx` and the CLI verbs don't cover a question, or when indexing a new repo.
Open DBs read-only: `sqlite3 -readonly DB "…"`. Decode ids with `symx name ID…`.

## symhow schema essentials (`.srctrldb`, Sourcetrail v25)

| Table | Use |
|---|---|
| `node(id, type, serialized_name)` | every symbol and file. `type` is a bit mask (below) |
| `edge(id, type, source_node_id, target_node_id)` | unique per (source, target, type); the multiplicity lives in `occurrence` |
| `occurrence(element_id, source_location_id)` | ties a node, **edge**, or local symbol to a code span. An edge's occurrences = its call/use sites |
| `source_location(id, file_node_id, start_line, start_column, end_line, end_column, type)` | loc type: 0 TOKEN, **1 SCOPE (whole body)**, 2 QUALIFIER, 3 LOCAL_SYMBOL, 6 INDEXER_ERROR |
| `file(id, path, indexed, …)` | `file.id` == the file's `node.id`. Paths are absolute (ucx: uncanonical `contrib/../src`) |
| `symbol(id, definition_kind)` | 2 = defined/declared in an indexed file; no row = external / reference-only |
| `filecontent(id, content)` | full text of indexed files |
| `error(id, message, fatal)` | unique diagnostics; `translation_unit` is always empty |
| `symhow_tu(path, status, …)` | one row per CDB TU (`ok`/`failed`) |
| `meta` | `symhow_repo_root`, `symhow_repo_commit`, `symhow_cdb_path`, … (**never `SELECT *`**: one value is ~MBs) |

Node kinds: 1 UNKNOWN (the single shared "unsolved symbol" hub, so exclude it from graph math), 2 TYPE, 16 NAMESPACE, 64 STRUCT, 128 CLASS, 1024 GLOBAL_VARIABLE, 2048 FIELD, 4096 FUNCTION, 8192 METHOD, 16384 ENUM, 32768 ENUM_CONSTANT, 65536 TYPEDEF, 262144 FILE, 524288 MACRO, 1048576 UNION.

Edge kinds (source → target): 1 MEMBER (container → member, no occurrences), 2 TYPE_USAGE, 4 USAGE (field/global access, **address-of a function**), 8 CALL (also *through* a fn-pointer field → FIELD node), 16 INHERITANCE (derived → base), 32 OVERRIDE (derived method → base method; **absent inside class templates**), 256 INCLUDE (includer → included), 2048 MACRO_USAGE (file → macro).

`serialized_name`: `<delim>\tm` then elements joined by `\tn`, each `name\tsPREFIX\tpPOSTFIX` (postfix = parameter list, plus ` [file.cpp#hash]` for TU-local symbols). File nodes use delim `/`. A quick SQL-only label (good enough to eyeball):
`replace(replace(replace(replace(n.serialized_name, char(9)||'m',''), char(9)||'s',''), char(9)||'p',''), char(9)||'n','::')`

## Verified recipes (folly DB, ms-level)

```sql
-- Enclosing function of FILE:LINE (innermost SCOPE)
SELECT o.element_id, l.start_line, l.end_line FROM file f
JOIN source_location l ON l.file_node_id=f.id JOIN occurrence o ON o.source_location_id=l.id
JOIN node n ON n.id=o.element_id
WHERE f.path LIKE '%/folly/io/async/AsyncSocket.cpp' AND l.type=1 AND l.start_line<=3961 AND l.end_line>=3961
  AND (n.type & 12288)!=0 ORDER BY l.end_line-l.start_line LIMIT 1;

-- Everything referenced on a line range, exactly resolved (edge kind + target)
SELECT l.start_line, l.start_column, e.type, e.target_node_id FROM file f
JOIN source_location l ON l.file_node_id=f.id JOIN occurrence o ON o.source_location_id=l.id
JOIN edge e ON e.id=o.element_id
WHERE f.path LIKE '%/folly/io/async/AsyncSocket.cpp' AND l.start_line BETWEEN 3955 AND 3965;

-- Transitive overriders of a method id
WITH RECURSIVE ov(id) AS (SELECT 64459 UNION
  SELECT e.source_node_id FROM edge e JOIN ov ON e.target_node_id=ov.id WHERE e.type=32)
SELECT id FROM ov WHERE id<>64459;

-- Transitive includers of a header + number of TUs affected
WITH RECURSIVE inc(id) AS (SELECT id FROM file WHERE path LIKE '%/folly/io/IOBuf.h' UNION
  SELECT e.source_node_id FROM edge e JOIN inc ON e.target_node_id=inc.id WHERE e.type=256)
SELECT count(*), (SELECT count(*) FROM symhow_tu t WHERE t.path IN (SELECT path FROM file WHERE id IN inc)) FROM inc;

-- Callers of every overload / instantiation sharing a name (what --all-matches does)
SELECT DISTINCT e.source_node_id FROM edge e JOIN node n ON n.id=e.target_node_id
WHERE e.type=8 AND n.serialized_name LIKE '%safe_assert_terminate_w%operator()%';

-- Call graph with call-site multiplicity, unsolved hub excluded
SELECT e.source_node_id caller, e.target_node_id callee, count(o.source_location_id) sites
FROM edge e LEFT JOIN occurrence o ON o.element_id=e.id
WHERE e.type=8 AND e.target_node_id NOT IN (SELECT id FROM node WHERE type=1)
GROUP BY e.id ORDER BY sites DESC LIMIT 20;

-- Functions with the most unresolved (template-dependent) call SITES = where the graph is blindest
-- (edges are unique per (src,dst,type): count the edge's occurrences, not edges)
SELECT e.source_node_id, count(o.source_location_id) sites FROM edge e JOIN occurrence o ON o.element_id=e.id
WHERE e.type=8 AND e.target_node_id IN (SELECT id FROM node WHERE type=1)
GROUP BY e.source_node_id ORDER BY sites DESC LIMIT 15;

-- Index errors per file (join through the occurrence; translation_unit is always empty)
SELECT f.path, count(*) c FROM error er JOIN occurrence o ON o.element_id=er.id
JOIN source_location l ON l.id=o.source_location_id JOIN file f ON f.id=l.file_node_id
GROUP BY f.path ORDER BY c DESC LIMIT 15;

-- Candidate dead TU-local functions (expect >=75% false positives: macros/templates/callbacks — grep each)
SELECT n.id FROM node n JOIN symbol s ON s.id=n.id
WHERE n.type IN (4096,8192) AND s.definition_kind=2 AND n.serialized_name LIKE '%#%]%'
  AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.target_node_id=n.id AND e.type IN (4,8))
  AND NOT EXISTS (SELECT 1 FROM edge e WHERE e.source_node_id=n.id AND e.type=32) LIMIT 50;

-- Body text of a function without opening the file (SCOPE span over filecontent)
SELECT l.start_line, l.end_line, f.path FROM occurrence o JOIN source_location l ON l.id=o.source_location_id
JOIN file f ON f.id=l.file_node_id WHERE o.element_id=67563 AND l.type=1;
```

## symwhere schema (`symwhere.db`)

`files(id, path [repo-relative], lang, hash, …)`, `symbols(file_id, name, kind, container, start_line, start_col, end_line, end_col, signature)`. Lines are 1-based, columns 0-based. It's useful for bulk questions:

```sql
-- All methods named X across the repo with body length, including tests and #if-disabled code
SELECT f.path, s.start_line, s.end_line-s.start_line+1 AS len, s.container FROM symbols s JOIN files f ON f.id=s.file_id
WHERE s.name='addInput' AND s.kind IN ('method','function') AND s.end_line>s.start_line ORDER BY len DESC LIMIT 10;
-- Largest functions in a directory (refactoring / review targets)
SELECT f.path, s.name, s.end_line-s.start_line+1 AS len FROM symbols s JOIN files f ON f.id=s.file_id
WHERE f.path LIKE 'velox/exec/%' AND f.path NOT LIKE '%/tests/%' AND s.kind IN ('method','function') ORDER BY len DESC LIMIT 10;
```

## Building indexes for a repo (lessons from folly / velox)

1. **compile_commands.json**: configure with `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` inside the repo's flox env. If the flox env dir lives inside the source tree, pass `CMAKE_PREFIX_PATH=$(readlink -f $FLOX_ENV)`, otherwise CMake rejects include dirs "prefixed in the source directory".
2. **Index with the C++ indexer, run directly** (not `flox activate`: activation can hang on a network call). The Rust indexer has a pathological case: a single TU (e.g. `velox/type/Conversions.cpp`) took >15 min (still running when killed) versus ~6 s with the C++ build at `symhow/cpp/build/symhow`. Use Rust only for queries and `graph`.
   ```bash
   # The C++ indexer needs clang's resource dir (…/lib/clang/<ver>) for the libclang symhow was built
   # against. Point $SYMHOW_RESOURCE_DIR at it (find it in your flox/toolchain env), and run the binary directly.
   SYMHOW_RESOURCE_DIR=<clang-resource-dir> env -u CPATH -u CPLUS_INCLUDE_PATH symhow/cpp/build/symhow index \
     -c CDB -o OUT.srctrldb --exclude '**/_deps/**' --exclude '**/tests/**' --exclude-submodules -j 12 --force --json -q
   ```
   (If you must use the Rust indexer, also set `LIBCLANG_PATH=<libclang lib dir>`.) Unset `CPATH`: shells auto-activate other repos' flox envs, and libclang treats `CPATH` entries like `-I` (ahead of the CDB's `-isystem`, and **not** as system dirs). In the first velox index, 836 folly/glog/boost headers from folly's env were therefore indexed *as velox project code* (+44k symbols, +1.9M refs of noise). It can also silently swap in a different library version.
3. **Nix compiler wrappers inject flags that never reach the CDB**, namely libstdc++ and glibc paths. Without help, libclang falls back to the *host* GCC (`/usr/include/c++/13` on this machine). Pass the build's GCC prefix, run inside the repo's flox env:
   `--gcc-toolchain $(c++ -print-search-dirs | sed -n 's|^install: \(.*\)/lib/gcc/.*|\1|p')` (velox: `/nix/store/…-gcc-16.2.0`). It's a small effect (velox errors 434 → 384), but it's the correct stdlib.
4. **Smoke-test before the full run**: index ~50 TUs (`--include` globs) and look at the top errors:
   `sqlite3 OUT "select substr(message,instr(message,'error:'),80) m, count(*) c from error group by m order by c desc limit 10"`.
   For velox this revealed glog 0.7 needing `-DGLOG_USE_GLOG_EXPORT` (velox's own `Findglog.cmake` never adds it), so the fix was `--extra-arg -DGLOG_USE_GLOG_EXPORT`, which cut errors 25×. Errors in subsystems whose dependencies aren't installed (velox parquet Arrow writer) are expected. Note them as blind spots.
5. Progress lines are printed **in TU order**. One slow TU freezes the counter while the other workers drain and go idle; check `top -H` (only 1–2 busy threads = a stuck TU). Find it with `--dry-run --limit N` (prints TUs in order) and time it alone with a one-entry CDB. Don't use `--include FILE` for that: it also restricts header walking.
6. **symwhere**: `symwhere index REPO --force -e '_build/**'` (`build/`, `target/`, hidden dirs are skipped already). **Keep tests in**: symwhere is the only lens that sees them. Then, for C++ repos only, run `scripts/symwhere-h-as-cpp.py DB REPO` (it takes ~25–50 s and roughly 2.5× the header symbols). Re-run it after every `symwhere update`.
7. Put the results where `symx` discovers them: `$SYMX_DBS/<repo>/how/<repo>.srctrldb` and `$SYMX_DBS/<repo>/where/symwhere.db` (`$SYMX_DBS` defaults to `~/explorer`). You can write symwhere straight there — `symwhere index -d $SYMX_DBS/<repo>/where/symwhere.db` — which also keeps `.symwhere/` out of the user's worktree.

### More lessons (sirius: pixi/conda toolchain, CUDA, unbuilt build dir)

Copy the sirius recipe for similar repos.

8. **Conda/pixi compilers** (`aarch64-conda-linux-gnu-c++`) use a nonstandard triple and their own sysroot, so `--gcc-toolchain` alone is ignored and libclang silently uses the host's GCC 13 libstdc++. Mirror what the env's own clang does (`$P/bin/*.cfg`): `--gcc-toolchain $P --extra-arg --target=aarch64-conda-linux-gnu --extra-arg --sysroot=$P/aarch64-conda-linux-gnu/sysroot`. Verify with `select path from file where path like '%/c++/%vector'`: it must be the env's GCC, never `/usr/include/c++/N`. `--extra-arg` takes the value as a **separate token** (`--extra-arg=X` is rejected).
9. **Configured-but-unbuilt build dirs lack generated headers.** Check every build-tree include dir in the CDB: `grep -oE '(-I|-isystem )<repo>/build/[^ "]*' CDB | sort -u` and count the files in each. Then regenerate *outside* the build dir and add it with `--extra-arg -I<dir>`. For sirius that meant a cxx bridge via `cargo build -p telemetry-bridge` with a private `CARGO_TARGET_DIR`, plus a `cmake -P embed_jit_headers.cmake` step. Errors fell 1235 → 644 in the smoke test. Put generated headers somewhere **stable** (e.g. `$SYMX_DBS/<repo>/generated/`): the DB stores those paths, and `symx` reads source from them.
10. **Dependency headers inside the repo that come in via `-I`** (pixi's `.pixi/envs/default/{include,targets/*/include}`: CUDA, RAPIDS, CCCL) are indexed as *project* code, about 16% of all symbols, plus their errors. Exclude them with `--exclude '**/.pixi/**'` (same for conda/venv/vcpkg dirs inside the repo). `select ... from file where indexed=1` grouped by top dir shows what was walked.
11. **Never pass symwhere globs through an unquoted variable**: the shell expands `dir/**` into filenames and clap fails. Build a bash array: `ex+=(-e "$s/**")`. Get submodule paths from `git config -f .gitmodules --get-regexp 'submodule\..*\.path'`. Also exclude untracked nested clones (`git status --porcelain` `??` dirs with their own `.git`) and gitignored vendored code.
