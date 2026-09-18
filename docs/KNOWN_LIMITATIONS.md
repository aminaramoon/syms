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
