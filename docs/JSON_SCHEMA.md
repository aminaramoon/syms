# kg JSON output schema

`kg query <verb> ... --json` and `kg index ... --json` print exactly one JSON
object to stdout (never split across lines, never mixed with any other
stdout output) and set the same process exit code text mode would have used
(0 ok, 2 ambiguous symbol, 3 not found, 4 db missing/unreadable, etc. -- see
`kg --help`). This document is the schema reference for that output,
implemented by `kg::out::json::JsonWriter` (`include/kg/out/JsonWriter.h`,
`src/out/JsonWriter.cpp`).

If a verb prints multiple results in text mode (`file` with `--all-matches`
against several matching paths), JSON mode prints multiple JSON objects too,
one per line (NDJSON-style) -- there is no wrapping array.

## Envelope philosophy

Every schema starts with the same two fields:

```json
{ "kg_version": "0.1.0", "schema": "kg.graph/1", ... }
```

- `kg_version` is exactly the version string `kg --version` prints
  (`KG_VERSION_STRING`), so a consumer can tell which `kg` produced a given
  file without shelling out separately.
- `schema` is one of the names below, always `"kg.<name>/<integer>"`. The
  integer is a schema version; a future incompatible change to a given
  schema's shape should bump it (`kg.graph/2`, etc.) rather than silently
  changing `kg.graph/1` out from under existing consumers.

Most (not all -- see the per-schema notes below) schemas also carry:

- `"query"`: an object echoing back what was actually asked (verb, db path,
  resolved symbol id(s), effective depth/kind filters/etc.) -- enough for a
  consumer to log or replay the request without re-parsing argv.
- `"stats"`: small, cheap-to-compute metadata about the result itself (counts,
  `elapsed_ms`). Never large data -- that always lives in a top-level array
  field (`nodes`, `hits`, ...), never nested inside `stats`.
- `"truncated"`: `true` only when the result was capped before every
  matching row/node could be included (see the per-schema notes -- several
  verbs structurally never truncate and always report `false`).

### Key ordering and determinism

This project uses plain `nlohmann::json` (not `nlohmann::ordered_json`)
everywhere, including `IndexDriver.cpp`'s pre-existing `--json` summary --
`JsonWriter` follows the same convention for consistency. That means object
keys serialize in **alphabetical order**, not insertion order; this is
deliberate and stable (every `dump()` of the same logical object produces the
same bytes), it just doesn't match the key order shown in the worked
examples below (those are written in whatever order reads best for a human).

Every array (`nodes`, `edges`, `hits`, `references`, `locations`,
`occurrences`, ...) preserves the exact order `kg::query::QueryEngine`
already produces -- `JsonWriter` never re-sorts anything itself. See
`docs/KNOWN_LIMITATIONS.md`'s Phase 4 section for the authoritative sort
contract (nodes by `(depth, kind, serializedName, id)`, edges by
`(kind, source, target, id)`, locations by
`(filePath, startLine, startColumn, fileId)`, search hits by
`(-score, textLength, displayName, id)`). Where JsonWriter itself introduces
a new deduplicated list not covered by that contract (`kg.references/1`'s
`nodes[]`, which flattens/dedups every hit's context+target ids), it applies
the same `(kind, serializedName, id)` tie-break for consistency.

`stats.elapsed_ms` is the one field that is NOT reproducible across
process invocations in general (it's a real wall-clock measurement) --
in practice, against a small database, it is almost always `0`, but a
byte-for-byte diff of two separately-invoked `kg query ... --json` runs is
only guaranteed to match if both runs measure the same `elapsed_ms`. Code
that needs strict byte-for-byte reproducibility (e.g. this phase's own
determinism unit test) calls `JsonWriter` directly with a fixed `elapsedMs`
argument rather than diffing two CLI invocations.

### The node object

Every schema that includes an actual graph node (`kg.graph/1`'s `nodes[]`,
`kg.references/1`'s `nodes[]`, `kg.definition/1`'s `symbol`, `kg.search/1`'s
`hits[].node`, `kg.ambiguous/1`'s `candidates[]`, `kg.not-found/1`'s
`suggestions[]`) uses the same object shape:

```json
{
  "id": 4711,
  "kind": "METHOD",
  "name": "get_policy_name",
  "qualified_name": "cucascade::memory::increase_reservation_limit_policy::get_policy_name",
  "signature": "std::string get_policy_name() const",
  "display_name": "helper() [tu_a.cpp#317677a3]",
  "indexed": true,
  "definition_kind": "EXPLICIT",
  "depth": 0,
  "seed": true,
  "location": { "file": "/path/to/file.hpp", "start_line": 164, "start_column": 15,
                "end_line": 164, "end_column": 29, "kind": "TOKEN" }
}
```

Field-by-field, in terms of `kg::db::DecodedName` (`include/kg/db/NameDecode.h`):

- `name` -- the bare leaf element name only (no ancestors, no signature):
  `decoded.elements.back().name`.
- `qualified_name` -- `decoded.namePath()`: the full `"A::B::C"` ancestor
  path, still with no signature/postfix.
- `signature` -- `decoded.signature()`: the last element's
  `prefix + " " + name + postfix` (return type + params).
- `display_name` -- `decoded.displayName()`, present **only** when it
  differs from `decoded.qualifiedName()`, i.e. only when Phase 3's hidden
  TU-local disambiguator suffix (` [tu_a.cpp#317677a3]`) is actually present
  on this symbol (a USR-collision candidate -- see
  `docs/KNOWN_LIMITATIONS.md`'s Phase 3 section). Absent entirely otherwise.
- `indexed` / `definition_kind` -- whether `node.id` has a `symbol` table row
  at all, and if so its `definition_kind` (`"EXPLICIT"`/`"IMPLICIT"`).
  `definition_kind` is JSON `null` (not an absent key) when `indexed` is
  `false`.
- `depth` / `seed` -- **only present for `kg.graph/1` nodes** (BFS-specific
  concepts); absent everywhere else (not `null` -- the key itself is
  omitted). `depth` is the BFS level (`0` for a seed node -- BFS results
  always include the seed(s) themselves at depth 0, see
  `docs/KNOWN_LIMITATIONS.md`). `seed` is `true` for every node that was
  actually passed to the traversal as a starting point (relevant with
  `--all-matches`, where there can be more than one).
- `location` -- the node's single best "definition-ish" location: SCOPE
  preferred over TOKEN, same selection `QueryEngine::definition()` uses,
  batched across every node in the result by `JsonWriter::hydrateNodes()`
  rather than one query per node. `null` (not an absent key) when the node
  has no recorded location at all (e.g. an `UNKNOWN`/unsolved-symbol node,
  or a system-header symbol with `--system-file-rows none`).

`kg.ambiguous/1`'s `candidates[]` and `kg.not-found/1`'s `suggestions[]` are
a deliberate exception: they're built directly from `kg::query::
ResolvedSymbol`/`SearchHit`'s own `{id, kind, serializedName}` rather than
re-querying the `node` table by id (`indexed`/`definition_kind` still come
from a `symbol` table lookup, `location` still comes from the usual
occurrence lookup) -- this matters specifically for **file-path** ambiguity
(the `file` verb): `ResolvedSymbol::serializedName` there holds the file's
raw `path`, not a real `serialized_name` (see `SymbolResolver.h`), and
re-fetching the actual `node` row for that id could show something
different. `kg.file/1`'s per-occurrence `node` field is intentionally
**minimal** (`{"kind": ..., "qualified_name": ...}` only) rather than the
full shape above -- see that section below.

### Location objects

```json
{ "file": "/path/to/x.cpp", "start_line": 90, "start_column": 12,
  "end_line": 90, "end_column": 26, "kind": "TOKEN" }
```

`kind` is a `kg::db::LocationKind` string (`TOKEN`, `SCOPE`, `LOCAL_SYMBOL`,
`UNSOLVED`, ...). Used for a node's `location`, an edge's `locations[]`
entries, `kg.references/1`'s `references[].location`,
`kg.definition/1`'s `locations[]`, and `kg.file/1`'s `occurrences[].location`.

## `kg.graph/1`

Every graph-producing verb: `callers callees bases derived members neighbors
overrides overridden-by`.

```json
{
  "kg_version": "0.1.0",
  "schema": "kg.graph/1",
  "query": {
    "verb": "callers",
    "db": "/tmp/cucs.srctrldb",
    "symbol": "cucascade::memory::increase_reservation_limit_policy::get_policy_name",
    "symbol_ids": [4711],
    "depth": 2,
    "edge_kinds": ["CALL"],
    "node_kinds": [],
    "max_nodes": 200
  },
  "truncated": false,
  "stats": { "node_count": 3, "edge_count": 2, "levels": 2, "elapsed_ms": 7 },
  "nodes": [ /* node objects, see above */ ],
  "edges": [
    { "id": 9001, "kind": "CALL", "source": 5120, "target": 4711,
      "locations": [ { "file": "/path/to/file.cpp", "start_line": 90, "start_column": 12,
                       "end_line": 90, "end_column": 26, "kind": "TOKEN" } ] }
  ]
}
```

- `query.edge_kinds` is the actual `kg::db::EdgeKind` value(s) this call
  used: a single fixed kind for `callers`/`callees` (`CALL`),
  `bases`/`derived` (`INHERITANCE`), `overrides`/`overridden-by`
  (`OVERRIDE`), `members` (`MEMBER`); for `neighbors` it's the user's
  `--edge-kind` filter, or (default, no filter given) every named edge kind
  decomposed from the `ALL` mask (`kg::out::json::decomposeEdgeKindMask()`).
- `query.node_kinds` is the raw `--kind` filter list, empty if unfiltered.
- `query.depth` / `query.max_nodes` reflect what the verb ACTUALLY applied:
  `callers`/`callees`/`bases`/`derived` use `--depth`/`--max-nodes`
  verbatim; `overrides`/`overridden-by`/`members`/`neighbors` are fixed at
  `depth=1`, `max_nodes=0` (unlimited) regardless of the flags (those verbs
  never apply a node cap at all -- see `QueryEngine.cpp`).
- `truncated` is `kg::query::BfsResult::truncated`. For `neighbors`
  specifically: its `QueryEngine::neighbors()` implementation builds its own
  `BfsResult` by hand (it isn't routed through `runBfs()`) and never sets
  `truncated` at all -- it is **always `false`** for that verb, since
  `neighbors` never applies a node cap. This is a real, permanent limitation
  of the current implementation, not a quirk of a particular query.
- `stats.levels` is the maximum `depth` among `nodes[]` (`0` if `nodes` is
  empty).
- `edges[].locations` is the sorted list of occurrence locations recorded
  for that specific edge id (an edge, not just its endpoint nodes, can have
  its own occurrence rows -- see `QueryEngine::references()`'s analogous
  edge-location lookup). It is `[]`, not omitted, when the edge kind simply
  has no recorded occurrence (this is expected/normal for some
  `INHERITANCE`/`OVERRIDE` edges).

## `kg.references/1`

```json
{
  "kg_version": "0.1.0", "schema": "kg.references/1",
  "query": { "verb": "references", "db": "/tmp/cucs.srctrldb",
             "symbol": "cucascade::idata_representation", "symbol_ids": [3001] },
  "truncated": false,
  "stats": { "reference_count": 2, "elapsed_ms": 4 },
  "nodes": [ /* full node objects for every id referenced in references[] (context+target), deduped */ ],
  "references": [
    { "edge_id": 8801, "edge_kind": "INHERITANCE", "context": 3110, "target": 3001,
      "location": { "file": "/path/to/disk_data_representation.hpp", "start_line": 37, "start_column": 42,
                    "end_line": 37, "end_column": 63, "kind": "TOKEN" } },
    { "edge_id": null, "edge_kind": "DECLARATION", "context": 3001, "target": 3001,
      "location": { "file": "/path/to/common.hpp", "start_line": 42, "start_column": 7,
                    "end_line": 42, "end_column": 27, "kind": "TOKEN" } }
  ]
}
```

**`edge_kind: "DECLARATION"` is invented by `kg`, not a real
`kg::db::EdgeKind` from the Sourcetrail schema.** It labels the one
`references[]` entry (`edge_id: null`) that is the symbol's own
declaration/definition site rather than an incoming reference edge --
`kg::query::ReferenceHit::hasEdge == false`. Every other entry's `edge_kind`
is a real upstream `EdgeKind` string (`CALL`, `INHERITANCE`, `MEMBER`, ...),
and `edge_id` is the real `edge.id`. For the `DECLARATION` entry, `context`
equals `target` (the symbol referencing itself, by convention, since there
is no separate "referencing" node for a declaration site).

`truncated` is always `false` -- `references()` has no cap/limit concept.
`nodes[]` is sorted by `(kind, serialized_name, id)` (see "Key ordering and
determinism" above), not by first-appearance in `references[]`.

## `kg.definition/1`

```json
{
  "kg_version": "0.1.0", "schema": "kg.definition/1",
  "query": { "verb": "definition", "db": "/tmp/cucs.srctrldb",
             "symbol": "fixture::Square", "symbol_ids": [23] },
  "truncated": false,
  "stats": { "location_count": 1, "elapsed_ms": 0 },
  "symbol": { /* full node object for symbol_ids[0] */ },
  "locations": [
    { "file": "/path/shapes.hpp", "start_line": 31, "start_column": 1,
      "end_line": 41, "end_column": 2, "kind": "SCOPE" }
  ]
}
```

A single `"symbol"` object, not a `nodes[]` array -- there's exactly one
resolved symbol per `definition` call. `locations[]` is sorted per Phase 4's
existing rule: SCOPE locations preferred over TOKEN (if any SCOPE location
exists for the symbol, only SCOPE locations are returned), then
`(filePath, startLine, startColumn)`.

**Caveat for `--all-matches`:** `definition` accepts multiple seeds (every
ambiguous candidate), but this schema's `"symbol"` field only reflects
`symbol_ids[0]` (the first seed) -- `"locations"` is still the full merged
set across every seed, since `QueryEngine::definition()` itself doesn't
track which location came from which seed (see
`docs/KNOWN_LIMITATIONS.md`'s Phase 4 section). A consumer combining
`definition --all-matches --json` output with more than one seed should not
assume every entry in `locations[]` belongs to `symbol`.

## `kg.search/1`

```json
{
  "kg_version": "0.1.0", "schema": "kg.search/1",
  "query": { "verb": "search", "db": "/tmp/cucs.srctrldb", "text": "reslimpol", "limit": 5, "kinds": ["CLASS"] },
  "truncated": false,
  "stats": { "candidates_scanned": 41233, "hit_count": 2, "elapsed_ms": 61 },
  "hits": [
    { "score": 47, "matched_indices": [19,20,21,32,33,34,38,39,40],
      "node": { "id": 3402, "kind": "CLASS", "name": "reservation_limit_policy",
                "qualified_name": "cucascade::memory::reservation_limit_policy",
                "signature": " reservation_limit_policy",
                "indexed": true, "definition_kind": "EXPLICIT",
                "location": { "file": "/path/memory_reservation.hpp", "start_line": 76, "start_column": 7,
                              "end_line": 76, "end_column": 31, "kind": "TOKEN" } } }
  ]
}
```

`stats.candidates_scanned` is a fresh `COUNT(*) FROM node [WHERE (type & ?)
!= 0]` mirroring `QueryEngine::search()`'s own candidate-selection WHERE
clause (that function doesn't return the count itself). `matched_indices`
are the fuzzy matcher's highlighted character positions into the node's
`name` (original, non-lowercased) -- see `kg::query::FuzzyHit`.
`truncated` is always `false`: `limit` is an intentional result cap, not a
"more results existed but were cut" signal -- there is no reliable way to
tell whether more matches existed beyond `limit` without re-running
unbounded, so this schema doesn't attempt to report it.

## `kg.stats/1`

```json
{
  "kg_version": "0.1.0", "schema": "kg.stats/1",
  "query": { "verb": "stats", "db": "/tmp/cucs.srctrldb" },
  "database": { "storage_version": 25, "path": "/tmp/cucs.srctrldb", "size_bytes": 184320000,
                "reader_indices_present": true },
  "counts": { "files": 1183, "files_indexed": 214, "nodes": 96421, "symbols": 71002,
              "edges": 214883, "source_locations": 402117, "occurrences": 431990,
              "errors": 37, "local_symbols": 812 },
  "node_kinds": { "NAMESPACE": 41, "CLASS": 1902, "FUNCTION": 12044, "UNKNOWN": 12 },
  "edge_kinds": { "MEMBER": 71002, "CALL": 40112, "INHERITANCE": 1204, "OVERRIDE": 388 },
  "errors": [ { "id": 771, "message": "...error text...", "fatal": false, "translation_unit": "/path/x.cu" } ]
}
```

No `"truncated"` key -- `stats` has no such concept. `node_kinds`/
`edge_kinds` are sparse (only kinds with a nonzero count appear).
`counts.local_symbols` is additive relative to the original worked example
in the Phase 5 design doc (not one of the 8 keys shown there) -- kept since
`StatsResult` already computes it and dropping real data seemed worse than
one extra key; every consumer should tolerate additional `counts` keys.
`errors[]` respects `--errors N` (default 100, same as text mode; **note:**
unlike what some internal design notes say, `--errors 0` is currently
treated the same as the default (100), not "show every error" -- this is
existing Phase 4 CLI behavior, unchanged by Phase 5), sorted by `id`.

## `kg.file/1`

```json
{
  "kg_version": "0.1.0", "schema": "kg.file/1",
  "query": { "verb": "file", "db": "/tmp/cucs.srctrldb", "path": "shapes.hpp" },
  "file": { "id": 3, "path": "/abs/path/shapes.hpp", "language": "cpp",
            "indexed": true, "complete": true, "line_count": 43 },
  "contents": "...",
  "occurrences": [
    { "location": { "file": "/abs/path/shapes.hpp", "start_line": 8, "start_column": 11,
                    "end_line": 8, "end_column": 18, "kind": "TOKEN" },
      "node": { "kind": "NAMESPACE", "qualified_name": "fixture" },
      "is_local_symbol": false },
    { "location": { "file": "/abs/path/shapes.hpp", "start_line": 34, "start_column": 28,
                    "end_line": 34, "end_column": 32, "kind": "LOCAL_SYMBOL" },
      "is_local_symbol": true }
  ]
}
```

`"contents"` is present only when `--contents` was given (and the file
actually has recorded content -- `filecontent` table). `"occurrences"` is
present only when `--occurrences` was given.

**`is_local_symbol` is invented by `kg`, replacing an implicit signal from
Phase 4.** `kg::query::FileOccurrence::elementKind == 0` means "this
occurrence's `element_id` is not a `node` row" -- which is true for BOTH an
edge occurrence (a reference token, not the symbol itself) AND a
`local_symbol` occurrence (parameters/locals/template parameters, which
never become graph nodes -- see `docs/KNOWN_LIMITATIONS.md`'s Phase 3
section). This schema makes that distinction explicit and self-documenting:

- `elementKind != 0` (a real node row): `"node": {"kind": ..., "qualified_name": ...}`
  is present (re-resolved from the actual `node.serialized_name`, using
  `namePath()` for `qualified_name` -- NOT the pre-decoded `displayName()`
  string `FileOccurrence::elementName` already carries, for consistency with
  every other `qualified_name` field in this document), `is_local_symbol` is
  `false`.
- `elementKind == 0` and the id IS present in the `local_symbol` table:
  `is_local_symbol` is `true`, no `node` key.
- `elementKind == 0` and the id is NOT in `local_symbol` (i.e. it's an
  edge/reference-token occurrence): `is_local_symbol` is `false`, no `node`
  key either. A consumer can distinguish this third case from a genuine node
  occurrence only by the absence of the `node` key.

## `kg.index-summary/1`

Printed by `kg index ... --json` (not `kg query`), replacing Phase 3's
ad-hoc flat-keyed `--json` summary object with the same envelope convention
as every other schema here. Same underlying data
(`kg::model::IndexSummary`), just reshaped -- no change to indexing logic.

```json
{ "kg_version": "0.1.0", "schema": "kg.index-summary/1",
  "database": "/tmp/cucs.srctrldb",
  "counts": { "tus_total": 200, "tus_attempted": 200, "tus_with_content": 190, "tus_failed": 10,
              "symbols": 71002, "references": 214883, "files": 1183, "errors": 37, "name_collisions": 0 },
  "elapsed_ms": 45210,
  "failed_tus": [ "/path/a.cu" ] }
```

No `"query"` field (there's no `kg query` verb/db-path pair for an index
run -- `"database"` is the output path directly).

## `kg.ambiguous/1`

Printed to **stdout** (replacing the text candidate table) before exiting
`2`, whenever `--json` is given and a symbol or file-path spec resolves
ambiguously without `--all-matches` -- both `kg query <verb> SYMBOL --json`
(symbol ambiguity, e.g. a class and its same-named constructor -- see
`docs/KNOWN_LIMITATIONS.md`'s Phase 4 section) and `kg query file PATH
--json` (multiple files matching a path suffix).

```json
{ "kg_version": "0.1.0", "schema": "kg.ambiguous/1",
  "query": { "verb": "bases", "db": "/tmp/cucs.srctrldb", "symbol": "Square" },
  "candidates": [ /* node objects, see "The node object" above -- built from
                     ResolvedSymbol directly, not re-queried from `node` */ ] }
```

Note: `query`'s positional-argument field is always called `"symbol"` in
this schema (and in `kg.not-found/1` below), even for the `file` verb's
path-ambiguity/not-found case (`query.symbol` then holds the raw path spec
the user typed, e.g. `"shapes.hpp"`) -- unlike `kg.file/1` itself, which
uses `"path"`. This keeps `kg.ambiguous/1`/`kg.not-found/1` a single,
verb-independent shape rather than branching field names by verb.

## `kg.not-found/1`

Printed to **stdout** (replacing the stderr "did you mean" text) before
exiting `3`. Two variants sharing one schema name:

- Symbol not found (`kg query <verb> SYMBOL --json`): `"suggestions"` is
  `QueryEngine::search(spec, nullptr, 5)`'s top-5 fuzzy hits, rendered as
  full node objects (same as `kg.search/1`'s `hits[].node`, minus the
  `score`/`matched_indices` wrapper -- these are just node objects here).
- File path not found (`kg query file PATH --json`): `"suggestions"` is the
  file verb's own `kg::query::fuzzyMatch()` result against the file map
  (`QueryEngine::search()` only ever ranks symbol nodes, not file paths, so
  this doesn't reuse that path) -- each suggestion is a node object with
  `kind: "FILE"`, `indexed: false`, and `qualified_name`/`name` both equal to
  the raw file path (files have no `serialized_name` to decode).

```json
{ "kg_version": "0.1.0", "schema": "kg.not-found/1",
  "query": { "verb": "bases", "db": "/tmp/cucs.srctrldb", "symbol": "NoSuchSymbolXYZ" },
  "candidates_scanned_note": "n/a -- see kg.search/1 for that stat",
  "suggestions": [] }
```

(The `candidates_scanned_note` line above is illustrative prose, not a real
field -- an empty `suggestions` array, as shown, is the normal/common case
when nothing fuzzy-matches either.)

## `kg.dbdiff/1`

Printed by `kg dbdiff A B --json`, a HIDDEN maintenance/equivalence-testing
command (not listed in `kg --help`) built on top of
`kg::update::{takeSnapshot,diffSnapshots}` (Phase 2 of `kg update`). It
compares two `.srctrldb` databases *logically* -- by content-derived keys
(`serialized_name`, paths, etc), never by row id, since ids aren't stable
across separate `kg index`/`kg update` runs of the same code -- and is how
Phase 2/3 verify that an incrementally-updated database is identical to a
full re-index. No `"query"` field (there's no `kg query` verb/db-path pair
here -- two database paths, not one, are the input).

```json
{ "kg_version": "0.1.0", "schema": "kg.dbdiff/1",
  "equal": false,
  "counts": { "only_in_a": 623, "only_in_b": 233 },
  "only_in_a": [ "node|::Square|4096", "..." ],
  "only_in_b": [ "node|::Kernel|4096", "..." ] }
```

Each entry in `only_in_a`/`only_in_b` is prefixed with its category
(`node|`, `edge|`, `location|`, `symbol|`, `file|`, `error|`,
`local_symbol|`, `symbol_def|`, `tu|`) followed by the "|"-joined key for
that category -- see `include/kg/update/Snapshot.h` for the exact field
list per category. Exit codes (not specific to `--json`): `0` the databases
are logically equivalent, `3` they differ, `4` one of the two databases
couldn't be opened at all (e.g. missing file, wrong `storage_version`) --
in that last case nothing is printed to stdout, only a plain-text error to
stderr (there's no well-formed `kg.dbdiff/1` object to emit, since one side
never got a Snapshot).

## `kg.update-summary/1`

Printed to **stdout** by `kg update ... --json` (Phase 3) on EVERY exit
code -- `0` (a real update, or `--dry-run`, which sets `"dry_run": true`
and reports what it *would* clear/re-index via the same fields a real run
uses), `5` (nothing to do; every count is `0` and `"index"` is an empty
object), `2` (refused) and `1` (hard error). The stderr plain-text message
is always printed too, same as before -- `--json` is additive, not a
replacement for it.

`--dry-run`'s object is the full envelope below with real (not `0`)
`"files"`/`"tus"` counts reflecting what WOULD be cleared/re-indexed --
`"gc"` and `"index"` stay at their zero/empty defaults (dry-run never
invalidates or re-indexes anything).

On a refusal (exit `2`) or hard error (exit `1`), the object is a smaller
"error envelope" instead of the full summary shape:

```json
{ "kg_version": "0.1.0", "schema": "kg.update-summary/1",
  "database": "/tmp/cucs.srctrldb",
  "error": "an update is already in progress on this database (kg_update_in_progress is set) -- ...",
  "exit_code": 2 }
```

(`"error"` is the same text printed to stderr, without the `kg update:
refused: `/`kg update: error: ` prefix; `"exit_code"` echoes the process's
actual exit code -- `1` or `2` -- so a caller parsing only stdout can still
tell success from failure without also capturing stderr.)

```json
{ "kg_version": "0.1.0", "schema": "kg.update-summary/1",
  "database": "/tmp/cucs.srctrldb", "dry_run": false, "in_place": false,
  "repo": { "root": "/home/x/cucs", "from": "a1b2c3d", "to": "e4f5g6h" },
  "files": { "changed": 3, "deleted": 1, "renamed": 1, "flag_changed": 0,
             "unmatched": 2, "cleared": 9, "include_closure": 5, "headers_pruned": 1,
             "cleared_paths": [ "/home/x/cucs/src/a.cpp", "..." ],
             "unmatched_paths": [ "/home/x/cucs/README.md" ] },
  "tus": { "to_index": 4, "new": 1, "retried": 0, "skipped_missing": 0 },
  "gc": { "files": 9, "nodes": 120, "edges": 340, "locations": 900,
          "local_symbols": 40, "errors": 2, "symbols_reset": 80,
          "non_indexed_files": 0, "orphan_elements": 3 },
  "index": { "tus_total": 4, "tus_attempted": 4, "tus_with_content": 4, "tus_failed": 0,
             "symbols": 71002, "references": 214883, "files": 1183, "errors": 37,
             "name_collisions": 0 },
  "elapsed_ms": 1234, "index_elapsed_ms": 1100 }
```

Notes:

- `"gc"."non_indexed_files"` is always `0` since Phase 5: the sweep that
  deleted edge-less non-indexed `file` rows was removed (a fresh `kg index`
  legitimately produces such rows -- see
  `docs/KNOWN_LIMITATIONS.md`'s Phase 5 section). The field is kept so the
  object's shape is unchanged.
- `"repo"."from"` is empty when `--files` was used (no git baseline
  involved at all).
- `"files"."renamed"` (Phase 4) is the number of `git diff`-detected
  `R<score>` rename entries in this run's change set (e.g. `git mv a.cpp
  b.cpp` counts as 1). `kg update` itself still processes a rename as an
  independent delete-old + add/index-new pair internally (`matchChanged()`
  doesn't special-case renames for invalidation purposes -- there's no
  "skip re-parsing, just relabel" fast path), so a rename's cost is the
  same as a delete+add; `"renamed"` exists purely as an accurate count of
  *how many* of `"deleted"`/`"changed"`/`"tus"."new"` were actually one
  side of a rename, for reporting. (Prior to Phase 4 this field was
  hardcoded to `0` -- a real gap, not a design choice; fixed alongside the
  git-driven scenario test that caught it, `test/update/update_scenario.cmake`.)
- `"files"."cleared_paths"`/`"unmatched_paths"` are always present (possibly
  empty arrays), even when `"dry_run"` or the "nothing to do" case leaves
  every count at `0`.
- `"tus"."to_index"`/`"new"`/`"retried"` all count CDB ENTRIES, not distinct
  files/paths -- almost always the same number, but NOT when the CDB lists
  a path more than once (e.g. the same source compiled into several
  targets/configs with different flags): re-indexing such a path
  re-indexes ALL of its entries together (kg_tu is keyed by path, so
  there's exactly one row either way, but every one of that path's
  entries still needs its own parse/walk pass), so these counts can exceed
  the number of distinct files actually touched. Same caveat applies to
  `kg.index-summary/1`'s `"counts"."tus_total"` and the human-readable
  `[k/N]` progress counter -- see `docs/KNOWN_LIMITATIONS.md`'s "Phase 9:
  duplicate CDB entries" section.
- `"tus"."retried"` (Phase 4) is the count of CDB entries whose previous
  `kg_tu.status` (see `include/kg/index/Provenance.h`'s `kKgTuDdl`) was
  `"partial"` or `"failed"` -- i.e. a TU that produced no content, had a
  fatal diagnostic, or (CUDA only) only succeeded via a degraded fallback
  rung last time. These are folded into the same clear-and-reindex set as
  genuinely changed files, so a healthy corpus reports `"retried": 0` on a
  repeat run that touches unrelated files (verified by
  `test_update_driver.cpp`'s "a second no-op run retries nothing" case).
  Prior to Phase 4, this used an over-broad heuristic ("no outgoing INCLUDE
  edge OR an INDEXER_ERROR occurrence") that retried every genuinely
  include-free TU on literally every run, forever -- see
  `docs/KNOWN_LIMITATIONS.md`'s Phase 3/4 sections.
- `"index"` mirrors `kg.index-summary/1`'s `"counts"` object exactly (same
  field names) when at least one TU was actually re-indexed; it's an empty
  object `{}` when `"tus"."to_index"` is `0` (e.g. a pure-deletion update,
  or nothing to do at all).
- No `"query"` field, same rationale as `kg.index-summary/1`/`kg.dbdiff/1`.
- `kg update` also refuses (exit `2`) if `<db>.kg-lock` already exists (another
  `kg update` is running, or a previous one crashed and left it behind -- the
  error message says to remove it if stale) or if a `<db>-journal`/`<db>-wal`
  file exists next to the database (a hot journal -- some previous writer may
  have crashed mid-write). The lock file is held (and removed, RAII-style) for
  the ENTIRE `runUpdate()` call, including the exit-`5` "nothing to do" path's
  best-effort provenance refresh.

## Text-mode truncation footer

Unrelated to JSON, but part of the same "surface `truncated` clearly" effort:
`kg::out::writeBfsResult()` (text mode) now prints an explicit
`"... truncated at N nodes"` line after the edge-count summary whenever
`BfsResult::truncated` is `true`, in addition to the existing inline
`[truncated: --max-nodes reached]` marker on the edge-count line itself.
