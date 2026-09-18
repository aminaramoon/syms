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

## Text-mode truncation footer

Unrelated to JSON, but part of the same "surface `truncated` clearly" effort:
`kg::out::writeBfsResult()` (text mode) now prints an explicit
`"... truncated at N nodes"` line after the edge-count summary whenever
`BfsResult::truncated` is `true`, in addition to the existing inline
`[truncated: --max-nodes reached]` marker on the edge-count line itself.
