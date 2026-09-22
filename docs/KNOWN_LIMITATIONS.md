# Known limitations

## Phase 1

- No indexing: `kg_index` is a stub target with no libclang-based indexing
  logic yet. `kg` only supports `--version` and `--help`.
- No querying: `kg_query` / `kg_out` are stub targets.
- No database read/write helpers beyond the vendored SourcetrailDB writer
  library itself (`kg_db` is a stub).
- `kg_integration_tests` is a placeholder executable; the real cucs-corpus
  integration test lands in Phase 6.
- `third_party/sourcetraildb` is built at C++17, not the project-wide
  C++20; see `third_party/sourcetraildb/PROVENANCE.md` for rationale.

## Phase 3

Real libclang-backed indexing (`kg index`, without `--dry-run`) is now
implemented (`kg::index::NameBuilder`, `TuParser`, `FileRegistry`,
`AstWalker`, `SrctrldbBackend`, `IndexDriver`), writing an actual
Sourcetrail-schema (storage_version 25) `.srctrldb` file. Known gaps/caveats
for the Phase 4 (read-only query engine) implementer:

- **Vendored-writer "unsolved symbol" node**: every `UNSOLVED` reference
  (e.g. a call/member-access dependent on an unresolved template parameter)
  is recorded via `SourcetrailDBWriter::recordReferenceToUnsolvedSymhol`,
  which internally creates a shared node named "unsolved symbol" that the
  vendored writer itself never assigns a `SymbolKind` to -- it is the ONE
  legitimate, unavoidable `node.type = 1` (`NodeKind::UNKNOWN`) row you
  should expect to see in any database that contains at least one unsolved
  reference (there is exactly one such node, shared by ALL unsolved
  references, since the vendored writer dedups nodes by `serialized_name`
  and every unsolved-symbol node has the same literal name). Any OTHER
  `node.type = 1` row is a real bug, not this.
- **Local symbols never become edge endpoints.** Parameters, local
  variables, and template parameters are recorded purely as
  `local_symbol` + `LOCAL_SYMBOL`-kind occurrences (highlighted together
  when one is clicked); they are never a `reference`/`edge` source or
  target. Don't expect to find call/usage edges into/out of a local symbol
  node -- there's no such thing in this schema.
- **File-kind "symbols" reuse the file's own node id.** `INCLUDE` edges and
  `MACRO_USAGE` reference contexts point at the exact same `node.id` that
  `file.id` uses for that path (there is no separate "file symbol" node);
  `SrctrldbBackend` special-cases this in `ensureSymbolNode()`.
- **Template instantiations collapse by display name.** `NameBuilder` does
  not include template argument lists in a symbol's `Name` (only function
  parameter *types* are included, for overload distinctness) -- so e.g.
  `std::vector<int>` and `std::vector<long>` both serialize to
  `::std::vector` and get merged into one node by the
  `SrctrldbBackend::ensureSymbolNode()` residual-collision safety net
  (`symbol.nameCollisions` in the `IndexSummary` counts these; expect a
  non-trivial number on template-heavy corpora like CCCL/thrust -- this is
  expected, not a bug).
- **`DefinitionKind::Implicit` is currently unused.** Only `Explicit` (via
  `clang_isCursorDefinition`) and `None` are ever produced; there's no
  detection of compiler-implicit special members (default ctor/dtor/copy,
  etc.) yet.
- **`clang_Cursor_getArgumentType` does not exist** in this libclang C API
  version; `NameBuilder`'s function-signature postfix uses
  `clang_Cursor_getArgument(c,i)` + `clang_getCursorType()` instead (same
  result).
- **`-resource-dir` semantics fixed in this phase**: `LibClang_RESOURCE_DIR`
  / `$KG_RESOURCE_DIR` now mean the *parent* of clang's builtin-header
  `include/` directory (matching `clang -print-resource-dir`), not the
  `include/` directory itself -- see `cmake/FindLibClang.cmake` and
  `.flox/env/manifest.toml` (the previous, one-level-too-deep convention
  silently broke parsing of any TU that included a system header, e.g.
  `<string>`; this was never exercised before Phase 3 since Phase 1/2 only
  ever printed argv in `--dry-run` mode).

## Phase 4

The read-only query engine (`kg query`) is now implemented: `kg::db::
Database`/`Enums`/`NameDecode`, `kg::query::Bfs`/`SymbolResolver`/
`FuzzyMatcher`/`QueryEngine`, and `kg::out::TextWriter`. JSON output is
explicitly out of scope (Phase 5). Notes for that implementer:

- **`runBfs()` always includes the seed nodes themselves in `BfsResult::
  nodes`, at `depth == 0`** (per its own documented contract: "hydrate every
  visited node ... 0 for seeds"). `QueryEngine::bases()`/`derived()`/
  `callers()`/etc. do NOT strip them back out -- a BFS-based verb's rendered
  tree shows the seed(s) as the root(s). This is deliberate (useful for text
  rendering: it's "the thing you asked about"), but it means "X's bases" as
  a SET literally includes X at depth 0; `test_query_fixture.cpp` filters to
  `depth > 0` when asserting "exactly {A,B,C}". If Phase 5's JSON shape wants
  a `results` array that's cleanly just the SEED's bases/derived/etc.
  excluding the seed, that filtering needs to happen at serialization time
  (or a new QueryEngine option added) -- it is NOT done today.
- **`resolveSymbol()`'s bare-leaf-name match can be legitimately Ambiguous
  in a way that surprises first-time users**: a class and its own
  constructor share the same leaf `.name` (e.g. `Square` the class node and
  `Square(double)` the constructor node both have leaf name "Square"), so
  `kg query bases Square` (no `--kind`) resolves Ambiguous, not Unique --
  verified against the real fixture. This is intentional/correct (a real
  name collision, shown to the user with kind + location so they can add
  `--kind CLASS` or `#<id>`), not a bug, but it's the one case in the
  fixture where a "plain English" query doesn't resolve on the first try.
- **`clang_getOverriddenCursors()` is NOT transitive**: it only reports the
  IMMEDIATE overridden method(s) in each direct base (see `AstWalker.cpp`'s
  `visitFunctionLikeDecl`). In the fixture's diamond-less multiple-inheritance
  hierarchy, `Square::area` overrides `Polygon::area` and `Colored::area`
  directly (NOT `Shape::area`); only `Polygon::area`/`Colored::area`
  themselves have a direct OVERRIDE edge to `Shape::area`. So exactly 2
  OVERRIDE edges target `Shape::area` directly, not 3 -- the "3" only shows
  up if you walk the OVERRIDE-edge subgraph transitively (unlimited depth),
  where you'll find all 3 overriding methods reachable. `QueryEngine::
  overriddenBy()` is fixed at depth=1 per its own contract (mirroring
  Sourcetrail's own "overridden by" UI, which shows one hop), so it only
  ever returns the direct overriders.
- **`DecodedName::qualifiedName()`/`displayName()`/`namePath()` are three
  deliberately different strings** (see `include/kg/db/NameDecode.h`):
  `namePath()` is the bare `"A::B::C"` path (names only, no signature);
  `qualifiedName()` appends the LAST element's postfix verbatim (so it
  includes both a function's parameter-list AND Phase 3's hidden TU-local
  disambiguator, when present -- this is what makes a USR-collision
  candidate's own `qualifiedName()` round-trip uniquely through
  `resolveSymbol()`); `displayName()` is `qualifiedName()` with just the
  disambiguator suffix stripped. `SymbolResolver::resolveSymbol()`'s exact-
  match check compares a spec against all three PLUS the bare leaf name
  (4 forms) so both "give me the fully-qualified/disambiguated thing" and
  "give me the plain unqualified thing" work as expected. If Phase 5's JSON
  schema wants a single canonical "name" field per node, `namePath()` is
  almost certainly the right default (it's what most tools call a
  "qualified name"); reserve `qualifiedName()`/`signature()` for a
  disambiguation-specific field.
- **The LIKE pre-filter in `resolveSymbol()` had to be narrowed to the
  leading identifier-shaped run of the spec's last segment**, not the whole
  segment: a spec that round-trips `qualifiedName()` (e.g.
  `"helper() [tu_a.cpp#317677a3]"`) glues the postfix directly onto the name
  with no delimiter, but the ON-DISK `serialized_name` has the writer's own
  `"\tp"`/`"\ts"` delimiter bytes in between -- so a naive `LIKE
  '%<wholespec>%'` silently finds nothing. Caught by
  `test_query_fixture.cpp`'s USR-collision round-trip test; fixed in
  `SymbolResolver.cpp`'s `likePrefilterKey()`. Worth remembering if Phase 5
  (or anyone) adds another spec-shaped LIKE query elsewhere.
- **`QueryEngine::definition()`'s SCOPE-vs-TOKEN preference is an "either
  the SCOPE rows, or (if none) the TOKEN rows" selection**, not a merged/
  interleaved list -- see the comment above the `chosen` variable in
  `QueryEngine.cpp`. The one-sentence spec for this verb ("prefer SCOPE,
  fall back to TOKEN, but if genuinely both exist return both") is
  ambiguous about whether "both" means "both kinds, mixed" or "both SCOPE
  rows, if there happen to be more than one" -- this implementation chose
  the latter reading. Flag for review if a real corpus surfaces a symbol
  where that choice actually matters (e.g. a template-collapsed node per
  the Phase 3 nameCollisions note above, which could plausibly have more
  than one SCOPE location).
- **Determinism**: every `QueryEngine` result list is explicitly sorted
  before being returned (nodes by `(depth,kind,serializedName,id)`, edges by
  `(kind,source,target,id)`, locations by `(filePath,startLine,startColumn,
  id/fileId)`, search hits by `(-score,textLength,displayName,id)`) --
  `runBfs()` itself also sorts its raw output the same way, so the two never
  disagree, but QueryEngine is the one whose sort order is the actual
  contract. `FileResult::occurrences` is sorted by `(startLine,startColumn,
  elementId)` (not literally one of the four generic list shapes above,
  since it's a per-file occurrence listing, not a node/edge/location/search
  list) -- worth a second look if Phase 5 wants it to match the others'
  shape exactly.
- **`ReferenceHit`/`FileOccurrence` can reference a non-`node` element_id**:
  an `occurrence.element_id` can point at an `edge` id (Sourcetrail records
  where a *reference* token appears, separately from where the referenced
  *symbol* appears) or a `local_symbol` id. `ReferenceHit::hasEdge`
  disambiguates the edge case explicitly; `FileOccurrence::elementKind == 0`
  is the (slightly implicit) "this wasn't a `node` row at all" signal for
  the file-occurrence case -- Phase 5 may want a cleaner tri-state enum
  instead of overloading `elementKind == 0` (0 is not a valid `NodeKind`
  value, so it's unambiguous today, just not self-documenting).

(Phase 5, the `--json` output layer, added no new entries here -- its own
implementation notes/quirks are documented inline in `docs/JSON_SCHEMA.md`
next to the relevant schema instead, e.g. the `display_name`-only-when-
different rule, the `DECLARATION`-is-synthetic note on `kg.references/1`, and
the SCOPE-vs-TOKEN `location` selection rule.)

## Phase 6 (final)

Final-phase work: a hermetic synthetic CUDA fixture test (always runs, needs
no CUDA toolkit), a real end-to-end integration test against the cucs
(cuCascade) corpus, and this documentation pass. `kg` is now considered
feature-complete against the original 6-phase design; see the project
status summary in the Phase 6 completion report for the full picture. Notes
for future maintainers:

### The hermetic CUDA fixture header (`test/fixtures/cuda/fake_cuda.h`)

Getting clang's `-x cuda -nocudainc -nocudalib --cuda-host-only` mode (the
"nowrapper" rung -- `kg::index::cuda::synthesize()`) to accept a
`kernel<<<grid,block>>>(args)` launch with zero real CUDA toolkit installed
took three iterations against clang directly (`clang -x cuda -nocudainc
-nocudalib --cuda-host-only -fsyntax-only -include fake_cuda.h kernels.cu`,
run standalone before anything was wired into `kg`/CTest -- see the Phase 6
completion report for the full transcript), converging on:

1. **`threadIdx`/`blockIdx`/`blockDim`/`gridDim` are NOT provided
   automatically** in this mode. `-nocudainc` specifically disables clang's
   own bundled CUDA wrapper headers (the ones that normally declare these as
   compiler-magic builtins) -- they must be hand-declared (as ordinary
   `__device__`-space `extern` globals of a hand-rolled `uint3`/`dim3`; no
   real storage/intrinsic backing is needed since `--cuda-host-only` never
   generates device code).
2. **The `<<<>>>` launch lowers to a call to `cudaConfigureCall(dim3, dim3,
   size_t sharedMem = 0, cudaStream_t stream = 0)`**, and this exact
   signature is required -- `size_t` (not `unsigned long`, even though
   they're the same underlying type on this platform), and BOTH trailing
   parameters defaulted (omitting the defaults produces "too few execution
   configuration arguments to kernel function call, expected 4, have 2",
   since `<<<grid,block>>>` only supplies 2 of the 4 arguments explicitly).
   Deleting the declaration reproduces "use of undeclared identifier
   'cudaConfigureCall'" directly (not `__cudaPushCallConfiguration`),
   empirically confirming this specific stub-header/bare-mode scenario uses
   the older `cudaConfigureCall`-shim lowering path, not the newer
   `__cudaPushCallConfiguration`/`__cudaPopCallConfiguration` ABI.
   **Caveat**: this is specific to the no-real-headers scenario. When a real
   `cuda_runtime.h` IS available (the Full rung against an actual CUDA
   toolkit -- see below), clang's own `__clang_cuda_runtime_wrapper.h`
   declares `__cudaPushCallConfiguration` instead, and THAT lowering path is
   used automatically; both paths produce a libclang cursor shaped
   identically for `AstWalker`'s purposes (next point), so this distinction
   doesn't matter for `kg`'s own correctness, only for what a stub header
   needs to declare.
3. **`size_t` itself needs `#include <cstddef>`** -- `-nocudainc
   -nocudalib` only disables CUDA-specific header search paths, not ordinary
   C++ standard headers, so this is a one-line fix, not a hand-rolled
   typedef.

A standalone libclang cursor dump (a throwaway ~60-line C program against
`clang-c/Index.h`, linked directly against the flox-provided `libclang.so`
-- `c-index-test` wasn't usable here, its cached prebuilt binaries in this
environment are linked against `libclang.so.13`, not this project's
`libclang.so.21`) confirmed that a `kernel<<<grid,block>>>(args)` call --
regardless of which of the two lowering paths above is active -- appears to
libclang as an ordinary `CXCursor_CallExpr` (clang's internal
`CUDAKernelCallExpr` AST node, a `CallExpr` subclass, has no distinct
libclang cursor kind) whose `clang_getCursorReferenced()` resolves DIRECTLY
to the kernel's `FunctionDecl`, with the `cudaConfigureCall`/
`__cudaPushCallConfiguration` call as a separate nested `CallExpr` child
(itself also recorded as its own, separate CALL edge). **This means
`kg::index::AstWalker::handleCallExpr()` needed zero CUDA-specific
special-casing** to record a real launcher-\>kernel `CALL` edge -- it was
verified to already work correctly by construction, not patched to handle
this case.

### Real cucs corpus CUDA outcome (as of this phase)

Toolchain versions at time of writing: `kg 0.1.0, libclang 21.1.8, sqlite
3.53.3, srctrldb v4.db25.p1` (`kg --version`); cucs's pixi-managed CUDA
toolkit is `Cuda compilation tools, release 13.2, V13.2.51` (`nvcc
--version`, `$KG_CUCS_DIR/.pixi/envs/default/bin/nvcc`). The flox
environment's `on-activate` hook auto-derives `KG_CUDA_PATH` from that same
nvcc's location when it's present.

Against the real corpus, `kg index`'s default `--cuda-mode auto` retry
ladder resolved the corpus's one `.cu` file
(`test/memory/test_gpu_kernels.cu`) on the **first rung (Full)** --
`-x cuda --cuda-path=<cucs's pixi CUDA toolkit> --cuda-gpu-arch=sm_120
--cuda-host-only`, i.e. clang 21 parsing the REAL `cuda_runtime.h` (via its
own compatibility wrapper, not the raw header) successfully despite the
toolkit (13.2) being considerably newer than clang 21's own CUDA-version
awareness. Both `__global__` kernels (`gpu_memory_work_kernel`,
`gpu_memory_verification_kernel`) and both `extern "C"` host launchers
(`performGpuMemoryWork`, `verifyGpuMemoryWork`) are indexed as `FUNCTION`
nodes with real `CALL` edges from launcher to kernel. One non-fatal error IS
recorded from this TU's parse (`__clang_cuda_runtime_wrapper.h:388:10:
error: 'texture_fetch_functions.h' file not found`), but it's attributed to
that clang-bundled wrapper header, not to `test_gpu_kernels.cu` itself, and
doesn't prevent the TU from producing content (`anyFatal` stays false for a
non-`CXDiagnostic_Fatal` severity diagnostic -- see `IndexDriver.cpp`'s
`tryRung()`). This result isn't guaranteed to reproduce on a different
machine/toolkit-version combination -- that's exactly why
`test/integration/test_cucs.cpp`'s assertion 10 is written as an XOR over
BOTH possible real-world outcomes (rung succeeds with a real CALL edge, or
every rung fails and the `.cu` file itself gets an `error` row) rather than
hardcoding today's specific outcome.

### Other things found while writing the integration test

- **`error.translation_unit` is always the empty string.** The vendored
  `SourcetrailDBWriter::recordError()` hardcodes `StorageErrorData(message,
  "", fatal, true)` -- the empty string IS `translation_unit`, unconditionally,
  for every error this vendored writer ever records (not something `kg`
  itself controls or could easily fix without patching vendored code). To
  determine which file an error actually belongs to, join through the
  occurrence it's given instead: `recordError()` always calls
  `addSourceLocation(errorId, location, LocationKind::INDEXER_ERROR)`, so
  `error.id = occurrence.element_id -> occurrence.source_location_id =
  source_location.id -> source_location.file_node_id = file.id` recovers the
  real file. `kg::query::StatsResult::errors`/`kg.stats/1`'s `errors[]`
  still surface the (always-empty) `translation_unit` field verbatim from
  the `error` table, since that's a faithful reflection of what's on disk --
  a consumer that needs the real file should do the same join, or `kg`
  could add a dedicated field for it in a future `kg.stats/2`.
- **Per-TU CUDA rung is not exposed via any JSON schema.**
  `kg::model::TuFactBatch::stats.parsedRungIndex` exists internally
  (`IndexDriver.cpp` uses it for the `-v` per-TU stderr progress line, e.g.
  `test_gpu_kernels.cu: ok (cuda rung 1)`) but isn't threaded into
  `kg.index-summary/1` or any other JSON schema. `test_cucs.cpp` recovers it
  today by grepping the captured `-v` stderr text for the `.cu` file's
  progress line, which works but is exactly the kind of thing a real
  machine-readable field would obsolete. Flagged as a nice-to-have
  follow-up, not implemented this phase (out of scope -- would need a
  `kg.index-summary/2` or a new `per_tu` array field).
- **The `disk_data_representation` / `reservation_limit_policy` /
  `idata_representation` line numbers cited in `docs/JSON_SCHEMA.md`'s
  worked examples and in the original Phase 6 design doc were re-verified
  against the live corpus and found still accurate** (76/113/131/149 for
  the base class and its three derived classes in
  `memory_reservation.hpp`; 104/122/140/164 for `get_policy_name`'s pure
  virtual declaration and its three overrides; 37/42 for
  `disk_data_representation`/`idata_representation`) -- and the corpus's TU
  count (200) likewise hadn't changed. Nothing needed adapting; noted here
  only because the design doc explicitly asked for this to be re-checked
  empirically rather than trusted.

## Phase 1 (incremental-update groundwork for a future `kg update`)

This phase does NOT add the `kg update` command -- it lands the write-path
correctness fixes and persisted state a future incremental indexer needs,
while every current `kg index` run stays a full index.

- **AstWalker version bumped to 2 (`kg::index::kWalkerVersion`).** The
  implicit "1" (no version existed before this phase) is bumped because of
  the DefinitionKind semantic change below -- a database produced by the
  old walker is not equivalent to one produced by the new one for the same
  inputs, so a future `kg update` must refuse to incrementally update a
  database whose `kg_walker_version` meta value doesn't match the running
  binary's (and fall back to a full re-index instead).
- **`DefinitionKind::Explicit` semantic change: a symbol's own declaration
  in an indexed file now counts as "defined", matching real Sourcetrail
  behavior.** Previously only `clang_isCursorDefinition()` (i.e., only an
  out-of-line `.cpp` definition, or an in-class definition) marked a symbol
  Explicit; a function *declared* in a header and defined out-of-line in a
  `.cpp` got no `symbol` row at all and showed as `[not indexed]` in `kg
  query search`/`kg query definition` -- a real, user-visible bug (see the
  `get_policy_name` example in the Phase 1 task description). Now every
  visited declaration/definition of a symbol in an indexed file marks it
  Explicit (`AstWalker::markDeclaredHere()`, called from `visitScopeDecl`,
  `visitFunctionLikeDecl`, `handleSimpleDecl`, `handleMacroDefinition`).
  This only ever *adds* `symbol` rows / upgrades definition_kind relative to
  the old behavior -- it can't make a previously-indexed symbol
  "not-indexed".
- **New kg-private tables** (created inside `SrctrldbBackend::finish()`'s
  post-write transaction, `CREATE TABLE IF NOT EXISTS`, so they're additive
  and harmless on a DB written by the real Sourcetrail indexer too):
  - `kg_symbol_def(node_id, file_node_id)` -- WITHOUT ROWID, PK on both
    columns: which indexed files declare/define each symbol, as observed in
    this run. Populated from `SymbolFact::declFiles` (set via the new
    `FactSink::declaredIn()`), resolved to real node ids, and written
    `INSERT OR IGNORE` so re-running never duplicates a pair. NOT
    necessarily complete for symbols with no declaration site kg visited at
    all (pure reference-only/system symbols) -- those simply have zero
    rows, which is expected, not a bug.
  - `kg_tu(path PRIMARY KEY, args_hash, file_node_id)` -- one row per
    indexed CDB entry. `args_hash` is a 64-bit FNV-1a hash (lowercase hex,
    16 chars, see `kg::index::fnv1aHex()` in `IndexDriver.cpp`) computed
    over `entry.directory + '\0' + join(entry.args, '\0')` on the
    REMAPPED-but-otherwise-raw `CompileEntry` (i.e., after `CompileDb::
    load()`'s remap/include/exclude step, but BEFORE `sanitize()` rewrites/
    drops/injects anything) -- this is deliberate: it makes the hash
    sensitive to a real CDB change (new/changed compiler flags) but
    independent of kg's own sanitizer version, which will keep changing
    across phases without the underlying compile command changing.
    `file_node_id` is the `file` row id for `entry.file` (looked up by exact
    string equality against `file.path` after the write; null if that TU
    produced no file row at all, e.g. a total parse failure).
- **Persisted provenance in `meta`** (`kg::index::Provenance`,
  `include/kg/index/Provenance.h`): `kg_provenance_version` (currently `1`),
  `kg_version`, `kg_walker_version`, `kg_cdb_path` (absolute), a single
  `kg_options_json` blob (remaps/include/exclude/system_file_rows/
  drop_args/extra_args/resource_dir/gcc_toolchain/std/cuda_path/cuda_mode --
  every sanitizer/CDB-loading option that affects indexed output),
  `kg_repo_root`/`kg_repo_commit`/`kg_dirty_paths` (via `git`, resolved by
  the CLI layer through the new `kg::common::process::run()` subprocess
  helper -- never a shell; empty/best-effort if git isn't available or the
  directory isn't a repo, which is a warning at `-v`, never a hard error),
  and `kg_indexed_at`/`kg_last_update_at` (ISO-8601 UTC; `kg_last_update_at`
  is always empty until a future `kg update` sets it). Written by
  `kg::index::writeProvenance()`/read by `readProvenance()`; a database
  indexed before this change has no `kg_provenance_version` row at all, so
  `readProvenance()` returns `std::nullopt` for it -- **such a database has
  no way to be incrementally updated later; it must be re-indexed from
  scratch once `kg update` exists.**
- **`kg index` gained `--repo DIR`** to pin the provenance repo root
  explicitly instead of relying on `git -C <dirname of -c PATH> rev-parse
  --show-toplevel` discovery.
- **Four write-path correctness fixes**, all in `SrctrldbBackend`:
  1. `consume()` now attaches range-less `ErrorFact`s to the TU's own main
     file node (`batch.stats.tuFile`, resolved via `ensureFileNodeByPath()`)
     instead of an arbitrary first file of the batch -- never `file_node_id
     = 0` anymore.
  2. OVERRIDE (`visitFunctionLikeDecl`) and INHERITANCE (`handleBase
     Specifier`) edges now go through the same null-spelling-range ->
     zero-width-point-range fallback (`AstWalker::spellingRangeOrPoint()`,
     refactored out of the two ad-hoc fallbacks that already existed for
     `handleDeleteExpr`/`handleTypeOrTemplateRef`) instead of silently
     dropping the occurrence when `clang_Cursor_getSpellingNameRange()`
     returns null. Invariant after this phase: every non-MEMBER edge kg
     emits has >= 1 occurrence (verified by a `[fixture]` test).
  3. `node.type`/`symbol.definition_kind` are no longer pushed through the
     vendored writer's `recordSymbolKind()` (unconditional `UPDATE`) /
     `recordSymbolDefinitionKind()` (first-write-wins `INSERT OR IGNORE`) at
     `finish()` time. Both are unsafe once a symbol can be re-observed
     across separate `kg index`/`kg update` runs: unconditional overwrite
     can downgrade a real kind back to a weaker fallback, and first-write-
     wins can never upgrade Implicit to Explicit on a later run. Instead,
     `finish()` now applies both via `kg::db::WritableDb` with explicit
     upgrade-only semantics: `node.type` only changes if the existing
     stored kind was Unknown/Type-fallback (never downgrades a real kind
     back to a fallback; equal-rank changes like class<->struct still
     apply), and `symbol.definition_kind` is `INSERT ... ON CONFLICT(id) DO
     UPDATE SET definition_kind = max(definition_kind, excluded.
     definition_kind)`.
- **New `kg_db` write-path infrastructure**: `kg::db::WritableDb` (RAII
  read-write `sqlite3*` wrapper mirroring `kg::db::Database`'s `run()`
  binder/row-callback style, plus `exec`/`begin`/`commit`/`rollback`/
  `changes`/`tableExists`) and `kg::db::Meta` (`getMeta`/`setMeta`/
  `deleteMeta`, where `setMeta` is DELETE-then-INSERT since `meta.key` has
  no UNIQUE constraint in the Sourcetrail schema). `kg::db::ensureIndices()`
  and `SrctrldbBackend::finish()`'s post-write fixup section were both
  migrated onto `WritableDb` (behavior-preserving refactor).
- **New `kg_common` subprocess helper**: `kg::common::process::run()`
  (`include/kg/common/Process.h`), a `posix_spawnp`-based runner (never a
  shell) used so far only for the `git` calls behind `kg index`'s
  provenance collection.

## Phase 2 (kg update: pure/testable pieces + equivalence testing)

Like Phase 1, this phase does NOT add the `kg update` command itself (that's
Phase 3) -- it adds the parts of it that can be built and unit-tested
without touching invalidation SQL: git diff/status parsing, changed-file
classification against a database/CDB, and a logical-equivalence snapshot
tool, all under a new `kg_update` static library
(`include/kg/update/`, `src/update/`).

- **`WritableDb::run()`'s two-overload footgun (see Phase 1's note, now
  removed) was fixed by splitting it into four unambiguous entry points**:
  `exec(sql)` (unparameterized, `sqlite3_exec`), `execBound(sql, Binder)`
  (bind + step, no rows expected), `query(sql, RowFn)` (no bind, read
  rows), `query(sql, Binder, RowFn)` (bind + read rows). There is no longer
  a `run()` member at all, so the old silently-ambiguous-but-compiling
  bind-vs-onRow call shape can't be written by accident. All Phase 1 call
  sites (`SrctrldbBackend.cpp`, `IndexDriver.cpp`, `Meta.cpp`,
  `test_writable_db.cpp`, `test_provenance.cpp`) were migrated.
- **`kg::update::GitDiff`** (`include/kg/update/GitDiff.h`): pure,
  process-free parsers for `git diff --name-status -z --find-renames`
  (`parseNameStatusZ`) and `git status --porcelain -z --untracked-files=all`
  (`parseStatusPorcelainZ`) output, verified byte-for-byte against real git
  2.55 output (see below) before being unit-tested against hand-written NUL
  strings.
- **`kg::update::Git`** (`include/kg/update/Git.h`): thin
  `kg::common::process::run()` wrappers (`revParse`, `topLevel`,
  `diffNameStatusZ`, `statusPorcelainZ`) plus `collectChanges()`, which
  combines a `git diff` and (when diffing against the working tree) a `git
  status` into one `ChangeSet{changes, dirtyPaths}` -- `git diff` alone
  never reports untracked files, so `collectChanges()` adds every `??`
  status entry as `ChangeKind::Added` itself. This duplicates (in spirit,
  not by sharing code) the git-shelling already inlined in
  `kg::cli::buildProvenance` (`src/cli/Cli.cpp`, from Phase 1) -- Phase 2
  does not refactor that existing code to use the new wrapper; a future
  phase could.
- **`kg::update::FileMatcher`** (`include/kg/update/FileMatcher.h`):
  classifies a `ChangeSet` against a target database's known TU paths and a
  freshly-loaded CDB into `changedInDb`/`newTus`/`deleted`/`unmatched`.
  Matching is always done via `normalizePath()` (absolute +
  `weakly_canonical()` if the path exists on disk, else `lexically_normal()`)
  on both sides, but every `MatchResult` field preserves the ORIGINAL
  spelling from whichever side matched (the DB's `kg_tu.path`/`file.path`
  spelling, or the CDB's `file` spelling) -- never git's spelling -- because
  Phase 3's invalidation SQL and `CompileDb::load()`-based re-indexing need
  to match by exact string equality against those tables/structures.
- **`kg::update::Snapshot`** (`include/kg/update/Snapshot.h`): a logical,
  id-independent snapshot of a `.srctrldb` (9 categories: nodes, edges,
  locations, symbols, files, errors, local_symbols, `kg_symbol_def`-derived
  symbol_defs, `kg_tu`-derived tus -- each a `std::set<std::string>` of
  "|"-joined content keys, never a sqlite row id) and `diffSnapshots()`,
  a symmetric set-difference used to verify that an incrementally-updated
  database is logically identical to a full `kg index` re-run. Built on
  ordinary read-only `kg::db::Database` queries; `kg_symbol_def`/`kg_tu`
  categories are silently empty (not an error) against a pre-Phase-1
  database that lacks those tables.
- **New hidden CLI command `kg dbdiff A B [--json] [--limit N]`** (dispatched
  in `src/cli/Cli.cpp` next to `index`/`query`, deliberately NOT listed in
  `kg --help`'s usage text): exit `0` if the two databases are logically
  equivalent, `3` if they differ (first `--limit` entries per side printed
  in text mode; full lists in `--json`), `4` if either database can't be
  opened at all. See `docs/JSON_SCHEMA.md`'s `kg.dbdiff/1` section.
- **Git output shape, verified empirically against git 2.55 (via `flox
  activate`) on this machine** (documented here since it's exactly what
  `GitDiff.cpp`'s parsers assume): `git diff --name-status -z` emits
  `STATUS\0path\0` per ordinary entry and `R<score>\0old\0new\0` /
  `C<score>\0old\0new\0` for renames/copies -- the status token itself is
  NUL-terminated, NOT tab-separated from the path as in the (very similar-
  looking) non-`-z` output. `git status --porcelain -z` emits `XY path\0`
  per entry (two status chars + one literal space + path, all before the
  NUL), with rename/copy entries as `XY new\0old\0` (two NUL-separated
  tokens) and untracked files as `?? path\0`.

## Phase 3 (kg update: invalidation + the command itself)

Adds the two remaining pieces of `kg update` to the `kg_update` library:
`kg::update::Invalidator` (`include/kg/update/Invalidator.h`,
`src/update/Invalidator.cpp`) -- the SQL that clears exactly the facts a set
of changed files invalidated, plus a post-index GC sweep -- and
`kg::update::UpdateDriver` (`include/kg/update/UpdateDriver.h`,
`src/update/UpdateDriver.cpp`) -- the orchestration that reads a database's
Phase 1 provenance, works out what changed (via git or `--files`), calls
the invalidator, and re-indexes only the affected TUs via
`kg::index::runIndex()`'s existing `appendToExisting`/`presetEntries`
machinery. `kg update` itself is now a real, non-hidden CLI subcommand
(`src/cli/Cli.cpp`).

- **Algorithm (all inside one `BEGIN...COMMIT`, temp tables, rollback +
  rethrow on any exception -- see `Invalidator.cpp` for the exact SQL)**:
  0. seed = `file` rows whose path is in the changed-or-deleted set.
  1. reverse-INCLUDE closure: any file that `#include`s a seed file is
     added too (run to a fixpoint -- transitively, an include of an
     include also gets pulled in).
  2. header pruning: a header (never a TU) all of whose includers are
     already in the clear set gets pruned too (also to a fixpoint) -- this
     is what makes clearing a leaf header also clear headers-that-only-
     that-header-used, and is why the `include/shapes.hpp` fixture test
     ends up clearing not just `shapes.hpp`/`ns.cpp`/`overloads.cpp` but
     also `macros.hpp` and even `<string>` (both are included ONLY by
     `overloads.cpp`/`ns.cpp`, both now cleared).
  3. candidates: every non-file element with an occurrence in a cleared
     file, closed over edge endpoints and MEMBER-edge ancestors (so e.g. a
     class defined in an untouched header but with one member defined in a
     cleared `.cpp` still gets reconsidered).
  4. definition-kind reset: a `kg_symbol_def` row is Phase 1's per-(symbol,
     declaring-file) bookkeeping; a symbol that loses every declaring file
     this way has its `symbol` row DELETED (not `UPDATE`d to
     `definition_kind=0`/`NONE`) -- deletion is the correct equivalent of "no
     definition anywhere", because a fresh `kg index` run never emits a
     `symbol` row for a node with `DefinitionKind::None` in the first place
     (see `kg::model::IndexBackend`'s writer); leaving a
     `definition_kind=0` row behind would make an incrementally-updated
     database diverge from a fresh index's `symbol` table shape, exactly
     the kind of drift `kg dbdiff`/`Snapshot` exist to catch.
     Also: after this step (and after the dead-node fixpoint further down),
     `invalidate()`/`sweepOrphans()` DELETE any `kg_symbol_def` row whose
     `node_id`/`file_node_id` no longer points at a live `node`/`file`, and
     NULL out any `kg_tu.file_node_id` that no longer points at a live
     `file` -- neither table has a real `FOREIGN KEY` (`kg_symbol_def` has
     none at all; `kg_tu.file_node_id` is a plain nullable column), so
     without this a dangling row would silently "inherit" a completely
     unrelated symbol/TU the next time sqlite reuses that dead rowid for a
     freshly re-indexed row.
  5. delete the cleared `file`/`element` rows themselves -- cascades take
     care of `filecontent`, their `source_location`s, and any
     INCLUDE/MACRO_USAGE edge touching them.
  6. delete candidate non-MEMBER edges left with zero occurrences, then
     purge orphaned `source_location` rows (`file_node_id` no longer a real
     node -- this also incidentally cleans up any legacy `file_node_id=0`
     row).
  7-9. dead-node/local-symbol/error/non-indexed-file/orphan-`element` sweep
     (`sweepOrphans()` reuses this, unrestricted, as a post-index belt-and-
     braces pass).
- **Why GC must run to completion BEFORE any re-indexing write**:
  `element.id` is a plain sqlite `INTEGER PRIMARY KEY` (an ordinary rowid),
  so a deleted id WILL be reused by the next `INSERT`. If invalidation and
  re-indexing interleaved (or GC were deferred), a freshly-written row could
  silently collide with a not-yet-GC'd stale row's id, corrupting the
  database in a way that would only show up as wrong query results, not a
  constraint violation. `UpdateDriver` therefore always fully commits
  `invalidate()` (and closes that `WritableDb`) before opening a fresh
  connection for `kg::index::runIndex()`.
- **The temp-copy naming/commit protocol**: by default `kg update` copies
  `<dir>/<stem>.incr-<pid><ext>` (NOT the real file), does all of the above
  against that copy, then `std::filesystem::rename()`s it over the original
  (same directory, atomic on a POSIX filesystem) only once indexing +
  post-index GC + `ANALYZE` have all succeeded. On any failure the temp
  copy (and its `.srctrlprj` sidecar -- the vendored writer derives that
  name via `rfind('.')` on the WHOLE output path, so the temp copy gets its
  own `<stem>.incr-<pid>.srctrlprj`, cleaned up alongside it) is deleted;
  the real database is never touched. `--in-place` instead sets
  `kg_update_in_progress=1` on the real file directly before doing any of
  the above and clears it on success; if `kg update --in-place` is killed
  mid-run, the next `kg update` (in-place or not) refuses to run at all
  until the database is re-indexed from scratch (there is deliberately no
  automatic recovery from a partially-invalidated, never-reindexed
  in-place database).
- **Refusal conditions (exit 2)**: no provenance at all (pre-Phase-1 db, or
  one written by the real Sourcetrail indexer); stored
  `kg_walker_version` != this binary's `kWalkerVersion` (a walker semantic
  change makes old facts and new facts incomparable -- see Phase 1's note
  on `AstWalker`); stored `system_file_rows == all` (that mode gives every
  transitively-reachable system header its own `file` row, which the
  reverse-INCLUDE closure isn't validated against and could make pruning
  wildly over- or under-shoot); `kg_update_in_progress` set; no repo root
  available (neither `--repo` nor a stored `kg_repo_root`) AND no `--files`
  given. `kg update -c/--repo/--include/--exclude/--cuda-mode/
  --system-file-rows` overrides are accepted but print a warning that the
  resulting database will diverge from what a fresh `kg index` with those
  options would produce (the stored options are exactly what a fresh index
  would need, and `kg update` doesn't re-verify old facts against new
  options).
- **The flag-change/retry heuristics, and where incremental provably ≠
  fresh on the fixture, resolved**:
  - Flag change detection recomputes each CDB entry's `kg_tu.args_hash`
    recipe (moved from a private helper in `IndexDriver.cpp` into
    `kg::index::computeArgsHash()`, `CompileDb.h`/`.cpp`, so `IndexDriver`
    and `UpdateDriver` share exactly one implementation) and treats a
    mismatch as a Modified change, and a `kg_tu` row whose path has no CDB
    entry anymore as a Deleted change.
  - The "retry" heuristic (files whose node has an `INDEXER_ERROR`
    occurrence, or NO outgoing INCLUDE edge at all) is intentionally
    conservative but provably OVER-broad: a TU with genuinely zero
    `#include`s (in the fixture: `chain.cpp`, `templates.cpp`, `tu_a.cpp`,
    `tu_b.cpp`) has no outgoing INCLUDE edge for a perfectly innocent
    reason and gets retried on EVERY `kg update` run, not just ones where
    it actually changed. This was verified empirically (`kg update --files
    chain.cpp` always also re-indexes those other three files) and is
    deliberately left as-is rather than "fixed" by, say, tracking a
    per-TU success flag: it's idempotent (re-indexing unchanged source
    reproduces identical facts, confirmed by the `dbdiff`-empty fixture
    tests) and only costs extra work, never correctness, and a more
    precise signal (e.g. a dedicated `kg_tu.ok` column) is a natural Phase
    4/5 follow-up rather than a Phase 3 blocker.
  - **A real, load-bearing bug this surfaced and fixed**: `matchChanged()`
    (Phase 2) only classifies a changed path against `kg_tu` paths and CDB
    entries -- by design, a changed HEADER with no CDB entry of its own
    always lands in `unmatched`. The first `--files shapes.hpp` cut of
    `UpdateDriver` fed ONLY `match.changedInDb` into `computeClearSet()`'s
    seed, so a changed header was silently dropped from the seed entirely
    and nothing downstream of it was ever cleared (the reverse-INCLUDE
    closure had nothing to close over). Fixed by resolving every entry in
    the raw `changes` list (not just `matchChanged()`'s TU-only
    classification) against `file.path` via `normalizePath()` and feeding
    every match into the seed directly -- `computeClearSet()`'s SQL matches
    its seed against `file.path` with an EXACT string comparison, so this
    resolution step also fixes the (much rarer) case where `kg_tu.path`'s
    spelling disagrees textually with `file.path`'s.
- **Docs**: this section; `docs/JSON_SCHEMA.md`'s new `kg.update-summary/1`
  section; `README.md`'s new `### Updating` subsection.

Left for Phase 4/5: a git-driven scenario test with real commits (edit a
`.cpp`, edit a shared header, delete a file, add a file, rename a file, a
compiler-flag-only change) rather than Phase 3's `--files`-only fixture
tests; `--in-place` end-to-end coverage; a less over-broad retry signal
(e.g. persisting per-TU success in `kg_tu` itself); polishing the
`### Updating` README section once the git-driven scenarios exist to
document against.

## Phase 4 (kg update: precise retry tracking + a real git-driven scenario test)

Replaces Phase 3's over-broad "no outgoing INCLUDE edge, or an
INDEXER_ERROR occurrence" retry heuristic with exact per-TU status
tracking, and adds the git-driven acceptance test Phase 3 explicitly left
open (`test/update/update_scenario.cmake`).

- **`kg_tu` gained a `status` column** (`kg::index::kKgTuDdl`,
  `include/kg/index/Provenance.h`): `'ok'` (produced content, no fatal
  diagnostic, and -- for a CUDA TU resolved via the Auto retry ladder --
  the winning rung was the first one tried, not a degraded fallback),
  `'partial'` (content but >=1 fatal diagnostic, or a CUDA TU that only
  succeeded on a degraded fallback rung), `'failed'` (no content at all).
  Populated in `IndexDriver.cpp`'s main indexing loop (index-aligned with
  `entries`, passed into `writeProvenanceAndTus()` as `tuStatuses`) from
  the same `hasContent()`/`stats.diagFatal`/`rungIndex` values that were
  already being computed for the per-TU `-v` stderr line and
  `kg.index-summary/1`'s `failed_tus`/`tus_failed` counts -- no new
  computation, just persisted per-path instead of discarded.
- **`kg::update::UpdateDriver`'s retry set is now `kg_tu` rows with
  `status != 'ok'`**, replacing the two-clause heuristic (`filesWithIndexerError`/
  `filesWithOutgoingInclude`, both removed). This fixes the exact bug Phase
  3 documented and deliberately left unfixed: a genuinely include-free TU
  (`chain.cpp`, `templates.cpp`, `tu_a.cpp`, `tu_b.cpp` in the synthetic
  fixture) no longer gets retried on *every* `kg update` run forever --
  only when its own previous run actually produced partial/no content.
  Verified by `test_update_driver.cpp`'s "a second no-op `--files
  chain.cpp` run retries nothing" case (`tus.retried == 0`, `tus.to_index
  == 1` on the second run, where a Phase-3 `kg update` would have reported
  `tus.retried == 3` -- the other three include-free TUs -- forever).
- **A database whose `kg_tu` table predates this column is treated as
  legacy and refused (exit `2`)**: `UpdateDriver` runs `PRAGMA
  table_info(kg_tu)` and checks for a `status` column before doing
  anything else with `kg_tu`; there is no in-place `ALTER TABLE ... ADD
  COLUMN` migration path (Phase 3's own `kg_tu` table -- `path`,
  `args_hash`, `file_node_id`, no `status` -- can't be retry-classified
  correctly after the fact, since the column simply doesn't exist yet for
  any row written before this phase). Re-index from scratch to pick up
  per-TU status tracking.
- **A real, load-bearing bug the scenario test caught**: `UpdateDriver`
  builds its `changes` list from up to three independent sources -- `git
  diff`/`git status` (`collectChanges()`), stored `kg_dirty_paths`
  reconciliation, and CDB-vs-`kg_tu` flag-change/deletion detection (args
  hash mismatch -> Modified, `kg_tu` row with no CDB entry -> Deleted).
  For a file that's BOTH really deleted via git AND absent from the CDB
  (true for every ordinary `git rm` of a tracked TU), the git-diff path and
  the CDB-flag-deletion path each independently pushed their own Deleted
  entry for the exact same path, so `files.deleted` (and the invalidation
  seed set derived from it) silently double-counted. The scenario test's
  step c (`git rm src/overloads.cpp`) caught this immediately:
  `files.deleted == 2`, not `1`. Fixed by de-duplicating `changes` by
  `(kind, normalizePath(oldPath), normalizePath(newPath))` right after all
  three sources have contributed, before any downstream counting or seed-
  set construction (`UpdateDriver.cpp`, just before the `renamedCount`
  computation below). The same de-dup also protects the (rarer, untested
  by the scenario -- see below) case of a TU whose content AND compile
  flags both change in the same commit, which would otherwise double-count
  as two separate Modified entries.
- **`files.renamed` was hardcoded to the literal `0`** in
  `kg.update-summary/1`'s JSON output (`UpdateDriver.cpp`, both the
  "nothing to do" and the normal-completion JSON blocks) -- a real gap,
  not a placeholder-by-design: `UpdateSummary` had no field tracking it at
  all. Fixed by adding `UpdateSummary::renamedCount` (counted from `git
  diff`-detected `ChangeKind::Renamed` entries in `changes`, after
  de-duplication) and wiring it into the JSON. `kg update` itself still
  processes a rename as an independent delete-old + index-new pair
  internally (`matchChanged()` isn't changed by this -- there's still no
  "just relabel, skip re-parsing" fast path for a pure rename with
  identical content), so this is a reporting fix only, not a performance
  one; see `docs/JSON_SCHEMA.md`'s updated `kg.update-summary/1` notes.
- **`test/update/update_scenario.cmake`**: a git-driven acceptance test
  (registered as the `update_scenario` CTest test, `LABELS "update"`,
  `TIMEOUT 600`, `SKIP_RETURN_CODE 77` if `git`/`sqlite3` aren't found at
  configure time), driving a real throwaway git repo (a copy of
  `test/fixtures/synthetic`, rewritten to `file(GLOB_RECURSE ...)` its
  sources so add/delete/rename need no `CMakeLists.txt` edits) through a
  commit sequence -- edit a `.cpp`, edit a shared header, delete a file,
  add a file, rename a file, a project-wide compiler-flag change, an
  uncommitted-then-reverted edit, "nothing changed", and a final
  `--in-place` update -- verifying after EVERY step that the incrementally
  updated database is logically identical to a full `kg index` re-run
  (`kg dbdiff`, exit `0`) and that `kg.update-summary/1`'s counts match
  what actually happened. Every step passed on first correct run except
  step c above (the deletion double-count bug, fixed, not worked around).
  Two subtleties worth flagging for a future reader of the script:
  - **Node-id stability is only asserted for a file untouched by the step
    in question.** A cleared file's symbols are deleted and re-created
    with fresh ids (no attempt to preserve old ids across a real
    clear+reindex of their own declaring file -- see Phase 3's note on why
    GC must fully commit before any re-indexing write), so `fixture::Shape`'s
    id is only checked stable across step a (which never touches
    `shapes.hpp`), and `level0`'s id (declared only in `chain.cpp`) is
    checked stable across steps b/c/d/e (none of which touch `chain.cpp`)
    but NOT across step f (the flag change touches every TU, including
    `chain.cpp` itself, so `level0` legitimately gets a new id there --
    re-baselined immediately after).
  - **`--in-place` end-to-end coverage** (final step) asserts
    `kg_update_in_progress` is absent from `meta` after a successful
    `--in-place` run (queried directly via `sqlite3`) and that the result
    still `dbdiff`s clean against a fresh `kg index` of the same commit.
- **Docs**: this section; `docs/JSON_SCHEMA.md`'s `kg.update-summary/1`
  section (`renamed`/`retried` semantics); `README.md`'s `### Updating`
  section (exit codes, the worktree-not-a-commit note, the legacy-DB
  refusal).

Remaining inexactness, deliberately not addressed this phase (candidates
for a future phase, not blockers):

- **TU-dependent header facts.** A fact that depends on WHICH TU a header
  was included from (e.g. a macro whose expansion differs by include
  order/definitions, or a symbol whose visible declaration differs under
  different `#ifdef` combinations across TUs) is reconciled the same way a
  full `kg index` reconciles it -- last-TU-processed-wins/merge, per
  `SrctrldbBackend`'s existing per-symbol upgrade-only rules -- but `kg
  update` only re-processes the TUs the invalidation closure decided were
  affected. If a header's *observed* facts differ by includer and only
  SOME includers get re-indexed in a given `kg update` run (e.g. because
  only one includer's own content changed, not the header's), the
  database can end up reflecting a mix of "this run's" and "a previous
  run's" per-includer observations for that header's symbols, rather than
  what a full re-index of every includer together would produce. Not
  observed on the synthetic fixture or scenario test (neither has
  TU-dependent header facts to exercise this with) -- flagged from reading
  the algorithm, not from a failing test.
- **Excluded-includer blind spot.** The reverse-INCLUDE closure (Phase 3's
  `computeClearSet()` step 1) only walks `file`/`INCLUDE`-edge rows that
  already exist in the database. A file excluded from indexing entirely
  (`--exclude`, or never matched by the CDB) that starts or stops
  including a changed header produces no signal `kg update` can see --
  there was never a `file` row or `INCLUDE` edge for it to begin with. This
  is inherent to the closure being DB-driven rather than filesystem/CDB-
  driven, and was already true in Phase 3 (not a Phase 4 regression);
  noted here because the scenario test's `add_compile_definitions`-based
  flag-change step made it easy to double check the *included* side stays
  exact.
- **`--system-file-rows all` stays refused (exit `2`), not supported
  incrementally**, unchanged from Phase 3: that mode gives every
  transitively-reachable system header its own `file` row, and the
  reverse-INCLUDE closure isn't validated against a database built that
  way (system headers rarely change, but the closure's fixpoint over a
  much larger `file`/`INCLUDE` graph hasn't been verified not to mis-prune
  or under-prune). Re-index from scratch is still the only supported path
  for a `--system-file-rows=all` database.

## Phase 5 (final): `kg update` validated against the real cucs corpus

Adds `test/update/cucs_update_scenario.cmake` (registered as the
`cucs_update_integration` CTest test, labels `integration;update`) --
`update_scenario`'s (Phase 4) real-git-commit correctness contract, but
against the REAL cucs corpus (the same 200-TU corpus `cucs_integration`
indexes fresh) over a real commit range in that repo's own history, inside
a throwaway `git clone --shared --no-checkout` (never the user's real
checkout). Two things validated at real scale: (a) `kg update` FROM..HEAD
reproduces a fresh `kg index` of HEAD exactly (`kg dbdiff`), and (b) a
single-`.cpp` `kg update` is dramatically faster than a full re-index. See
the script's own header comment for the exact clone/remap/symlink protocol
(cucs's CDB references two absolute roots -- `/mnt/workspace/oss/cucs` for
`.pixi`/toolchain paths and the real checkout path for source/`directory`
-- both remapped onto the clone; `.pixi` and `build` are symlinked into the
clone rather than copied, and gitignored via `.git/info/exclude` so `kg
update`'s worktree diff doesn't see them as permanently "added").
### What the real corpus actually showed, and the rules now in force

The first real-corpus runs were nowhere near equivalent: `kg dbdiff
inc.srctrldb full_head.srctrldb` reported **441 rows only in the
incrementally-updated database and 18 rows only in the fresh one**. Every
one of those was chased to a root cause by querying both databases
directly (no guessing); the findings, and the code that now encodes them:

**1. Three GC rules were deleting rows a fresh `kg index` legitimately
keeps** (the 18 "only in fresh" rows). The GC's problem is that after a
partial re-index it cannot tell "leftover of a fact nobody produces any
more" from "fact the current index genuinely produces", because the shapes
are identical. Measured on `full_head.srctrldb` (a fresh 200-TU index of
cucs):

```sql
-- nodes with no occurrence, no non-MEMBER edge and no outgoing MEMBER edge,
-- i.e. exactly what the dead-node sweep deletes: 6 rows, ALL legitimate
select count(*) from node n
 where n.id not in (select id from file)
   and not exists (select 1 from occurrence o where o.element_id=n.id)
   and not exists (select 1 from edge e where e.type<>1 and (e.source_node_id=n.id or e.target_node_id=n.id))
   and not exists (select 1 from edge e where e.type=1 and e.source_node_id=n.id);
```

Those 6 are implicit template instantiations such as
`cucascade::exec::future::then_try<(lambda at .../prefetching_cache.cpp:768:17)>`
and `std::filesystem::is_regular_file(const path&, error_code&)`:
`AstWalker::ensureSymbol()` registers a referenced symbol's whole
semantic-parent chain, so the node (and its `MEMBER` edge from its parent)
is created even when nothing ever anchors the node itself with an
occurrence. A fresh index keeps them; an unrestricted sweep deleted them.
The same applies to edge-less, occurrence-less **non-indexed `file` rows**
(`.pixi/.../kvikio/file_handle.hpp`, `.pixi/.../cudf/version_config.hpp`,
libstdc++'s `<random>`), which `FileRegistry`'s `--system-file-rows` policy
creates and the walker then never describes.

The fix is to give the sweep the one piece of information it was missing:
**what the indexing pass that just ran actually produces.**
`SrctrldbBackend::finish()` now writes a `kg_touched(node_id)` table from
`m_byKey`/`m_fileNodeIdByPath` (plus a MEMBER-ancestor closure, because the
vendored writer materializes intermediate `NameHierarchy` elements itself
and only hands back the leaf id, and plus the writer's shared "unsolved
symbol" node when this pass produced an unsolved reference). For a full
`kg index` that table is *every node in the database* -- asserted by
`test/unit/test_incremental_fixture.cpp` and by the cucs scenario itself --
so every rule below is a **provable no-op on a freshly indexed database**.
`invalidate()` empties `kg_touched` before the re-index, so a stale entry
can never protect a dead node. The rules now in force, all in
`deadNodeAndOrphanSweep()`:

| rule | predicate | why it is safe |
| --- | --- | --- |
| dangling non-MEMBER edge | `type<>1`, source is not a `file` row, source not in `kg_touched`, source has no occurrence | a fresh index has 0 such rows (asserted); the source symbol is one the current pass no longer produces, so its outgoing edges (which describe its body) are dead |
| dead node (fixpoint) | not a `file`, not in `kg_touched`, no occurrence, no non-MEMBER edge, no outgoing MEMBER edge, **and** restricted to clearing candidates | 0 rows on a fresh index (asserted); an unguarded version deletes 5-6 legitimate rows on this corpus (the scenario reports that number every run) |
| local symbols / errors | no occurrence | unchanged from Phase 3 |
| orphan `element` / orphan `source_location` | no backing `node`/`edge`/`local_symbol`/`error`; no backing `occurrence` | pure cleanup, invisible to `Snapshot` either way |
| ~~stale non-indexed `file` rows~~ | **removed** | a fresh index legitimately produces edge-less non-indexed file rows, and the re-index cannot be relied on to re-create one (its contributing TU may not be in the re-index set) |

The dead-node sweep is additionally **always** restricted to a candidate
set, never run over the whole database: `kg_cand` in `invalidate()`'s
pre-index pass, and a new `kg_pending_cand(serialized_name)` table in the
post-index `sweepOrphans()` pass. Candidates have to be carried across the
re-index **by `serialized_name`, not by id** -- `element.id` is a plain
sqlite rowid the re-index can hand out again -- so `invalidate()` persists
the candidate node names into a real (not `TEMP`) table and `sweepOrphans()`
consumes and drops it.

**2. An occurrence could land inside a file kg was told not to index** (the
`cuda.h` / `cuda_runtime.h` locations in the first run). `AstWalker::visit()`
gates *cursors* on `FileRegistry::Info::indexed`, but a cursor in an indexed
`.cpp` can still have a spelling/reference *range* that resolves into a
non-indexed header -- macro expansions resolve to the macro's definition
site, implicit instantiations resolve into the template's own header. The
rule now enforced is: **a fact is emitted only if its own range's file is
indexed** (`AstWalker::rangeFileIndexed()`, called from every handler that
creates a reference edge, plus a backstop inside
`emitOccurrenceForRange()`). Indexer errors are exempt -- they attach to the
TU's main file by design (`source_location.type = 6`). Asserted on fresh
databases (fixture + cucs) by:

```sql
select count(*) from source_location l join file f on f.id=l.file_node_id
 where f.indexed=0 and l.type<>6;   -- was 64, must now be 0
```

`kWalkerVersion` is bumped 2 -> 3 for this (one bump, covering all the
walker changes in this phase); a database indexed by an older walker is
rejected by `kg update` with a "re-index from scratch" error.

**3. `node.type` was last-writer-wins for equal-ranked kinds**, i.e.
dependent on TU processing order. `SrctrldbBackend::finish()`'s
upgrade-only `UPDATE node SET type=...` guard used `rank(stored) <=
rank(new)`; libstdc++'s `std::pair` is reached both as a `ClassTemplate`
(-> `CLASS`) and as a `StructDecl` (-> `STRUCT`), both rank 2, so a 200-TU
full index and a 1-TU `kg update` on top of it disagreed. Now `<` (first
real kind wins), with the virgin-`UNKNOWN` -> `TYPE` case kept explicitly.

**4. The corpus was indexing the CUDA toolkit and its vendored
dependencies as first-party code.** cucs's CDB reaches `.pixi/...` (CUDA,
cudf, rmm, kvikio) and `build/release/_deps/...` (Catch2) with plain `-I`,
not `-isystem`, so `clang_Location_isInSystemHeader()` is false and kg
walked ~20k-line toolkit headers from every one of the ~200 TUs. That is
kg's `-I`-vs-`-isystem` classification working as designed, but it is not
the realistic configuration for this corpus, and it accounted for the large
majority of the remaining divergence. `test/update/cucs_update_scenario.cmake`
now passes `--exclude '**/.pixi/**' --exclude '**/_deps/**'` to `kg index`;
`kg update` inherits them from the database's stored provenance
(`Provenance.cpp`'s `kg_db_options` meta), which the scenario exercises
implicitly. Worth considering (deliberately NOT changed here): defaulting
to treating headers outside the repo root / the CDB's `directory` tree as
non-indexed.

### The one residual divergence: occurrence locations in shared headers

With all of the above, the default `HEAD~1..HEAD` run is down to **42
differences, all of them `location` rows, all one-sided (nothing is ever
lost -- `only_in_b` is 0 and the scenario asserts that hard)**; the
`HEAD~3..HEAD` run is down to 2. They are all TYPE_USAGE occurrences inside
repo-local *shared* headers (`memory_space.hpp`, `memory_reservation_manager.hpp`,
...) whose own content did NOT change in the range.

This is inherent to kg's fact granularity, not a bug that can be swept
away. A fact's only provenance in the Sourcetrail schema is the **file its
`source_location` lives in** -- not the **TU that produced it**. Template
instantiation makes those two differ: walking `data_batch.cpp` records
TYPE_USAGE occurrences whose cursors land in `memory_space.hpp` (the
instantiated pattern's own text) with a context symbol from a third file
entirely. Verified by direct inspection: `memory_space.hpp:178:15` carries
5 distinct edges' occurrences in the FROM-indexed database and only 3 in a
fresh HEAD index, while the header itself is byte-identical at both
commits. When `data_batch.cpp` changes, `kg update` clears and re-walks
`data_batch.cpp`, but the occurrences its OLD content contributed *inside*
`memory_space.hpp` are not `data_batch.cpp`'s to clear: the header is
unchanged and still included by ~100 TUs that were correctly not
re-indexed, so clearing it would destroy THEIR facts instead. Fixing this
properly requires per-fact TU provenance (a row per occurrence per
producing TU) -- i.e. persisting the fully un-deduplicated fact volume,
an order-of-magnitude database-size increase -- and is explicitly out of
scope here.

**`kg dbdiff --ignore shared-header-locations`** was therefore added, and
is the *only* relaxation in the test. It drops `locations` rows whose file
is the target of `INCLUDE` edges from two or more distinct includers
(`SnapshotOptions::ignoreSharedHeaderLocations`, implemented in
`takeSnapshot()`). It is narrow by construction:

- it ignores **only** occurrence locations. `node`, `edge`, `symbol`,
  `file`, `error`, `local_symbol`, `symbol_def` and `tu` rows are never
  ignored, in either direction.
- it ignores them **only** inside multiply-included headers. A header with
  a single includer cannot carry this divergence at all: if its one
  includer is cleared, `computeClearSet()`'s header-pruning step clears the
  header with it.
- the scenario still runs the **strict, no-exclusions** diff first and
  fails hard if `only_in_b` is non-zero (`kg update` may never lose
  anything) or if any surviving difference is anything other than a
  `location` row. Only then does it accept the `--ignore` run, which must
  be exactly empty. The single-file speed run's `kg dbdiff` is still
  checked with **no** `--ignore` at all, and is exactly equivalent.

### Results

Default range (`KG_CUCS_UPDATE_FROM` unset -> `HEAD~1`), 200-TU corpus:

| measurement | value |
| --- | --- |
| `T_full_from` (full `kg index` @ FROM) | 87 s |
| `T_update` (`kg update` FROM -> HEAD, 46 TUs re-indexed) | 74 s |
| `T_full_head` (full `kg index` @ HEAD, dbdiff baseline) | 92 s |
| `T_small` (`kg update --files memory_reservation.cpp`) | 6 s |
| strict `kg dbdiff` | 42 only in A (all `location` rows in shared headers), 0 only in B |
| `kg dbdiff --ignore shared-header-locations` | equivalent |

`KG_CUCS_UPDATE_FROM=HEAD~3` (38 changed files, 56 TUs re-indexed):
`T_full_from` 84 s, `T_update` 76 s, `T_full_head` 85 s, `T_small` 5 s,
strict diff 2 `location` rows / 0 lost, `--ignore` run equivalent.

Note that `T_update` is only modestly below `T_full_head` for these ranges:
re-indexing 46 of 200 TUs costs ~80% of a full index because the changed
files are widely-included headers *and* because a full index parallelizes
across all TUs while an update has fewer to spread over. The single-file
case (`T_small`, the realistic edit-loop scenario) is the one that matters
and is ~15x faster than a full re-index -- the scenario asserts `T_small <
T_full_head` hard and warns below the `1/5` bar.

**Docs**: this section; `README.md`'s `## Testing` (`ctest -L update`,
`KG_CUCS_UPDATE_FROM`); `test/CMakeLists.txt`'s `cucs_update_integration`
registration comment.
