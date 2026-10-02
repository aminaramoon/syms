# kg notes — function-level knowledge attached to the symbol graph

## Goal

Let agents (and humans) attach durable, reviewable knowledge to functions —
*brief / what / how / signature / pre-conditions / post-conditions / bugs /
suggestions* — and retrieve it together with a function's callers and callees
in one call, so code does not have to be re-read in every session/worktree.

Design principles:

1. **Markdown in git is the source of truth.** Notes live under the repo
   (`<repo>/notes/...`), are versioned, diffable, reviewable, and shared across
   worktrees/sessions. The `.srctrldb` only holds a **derived cache**
   (`kg_note*` tables) that `kg notes sync` rebuilds from the markdown.
2. **Keyed by stable identity, never by `node.id`.** Node ids change whenever a
   file is re-indexed. Notes are keyed by the symbol's `serialized_name`
   (qualified name + signature), which is also the writer's own dedup key.
3. **Querying never writes the DB.** `kg query`/`kg notes show` detect a stale
   cache (a notes file changed on disk since the last sync) and overlay the
   re-parsed file **in memory**, printing a hint to run `kg notes sync`.
4. **Staleness is tracked automatically.** Each note stores a hash of its
   function's definition body (computed from `filecontent` + the SCOPE
   location kg already records). `kg update` recomputes hashes for noted
   symbols in re-indexed files and flags notes whose body changed.

## Markdown format (one file per source file)

Path: `<notes-dir>/<source path relative to repo root>.md`
(e.g. `notes/src/planner/query.cpp.md`). A function's note lives in the file
of its **definition** (SCOPE location; falls back to the declaration file).

```markdown
# src/planner/query.cpp

## sirius::planner::query::get_pipeline
<!-- kg-hash: 9f7553b7a8b8475637bf57fc0844705a  kg-commit: 0dca58628 -->
One-line brief. (free text until the first `###`)
### what ?
_TODO_
### how ?
_TODO_
### signature
```cpp
sirius::pipeline::sirius_pipeline *get_pipeline(op::sirius_physical_operator *)
```
### condition
#### pre-condition
1. _TODO_
#### post-condition
1. _TODO_
#### bugs
1. _TODO_
#### suggestions
1. _TODO_
```

Rules:
- `#` = file heading (informational). `##` = symbol block; heading text is the
  **qualified name** (kg `qualified_name`, i.e. `namePath()`, no parameter list).
- `### signature` holds the kg `signature()` string in a ```` ```cpp ```` fence;
  it is what disambiguates **overloads** (several `##` with the same heading).
  A block whose heading matches >1 symbol and whose signature does not match
  any is an **ambiguous** note (lint/sync error with line number).
- Section order is fixed; unknown `###`/`####` headings are a lint error;
  missing sections are allowed (treated as empty) but `lint --strict` flags them.
- `_TODO_` is the placeholder token. A section equal to the placeholder counts
  as **unfilled**; a note with all sections unfilled is "scaffolded, not
  annotated" in coverage reports.
- Parser is a fence-aware heading scanner (no tree-sitter): headings inside
  ```` ``` ```` fences are ignored; CRLF tolerated; line numbers kept for every
  block for error reporting and `notes_path#Lnn` links.

## Cache schema (kg-private tables; extra tables are harmless to Sourcetrail)

```sql
CREATE TABLE IF NOT EXISTS kg_note(
  serialized_name TEXT PRIMARY KEY,   -- stable symbol identity
  qualified_name  TEXT NOT NULL,
  notes_path      TEXT NOT NULL,      -- relative to notes dir
  heading_line    INTEGER NOT NULL,
  brief TEXT, what TEXT, how TEXT, signature_text TEXT,
  pre_json TEXT, post_json TEXT, bugs_json TEXT, suggestions_json TEXT,  -- JSON arrays of strings
  filled          INTEGER NOT NULL,   -- 1 if any section is non-placeholder
  body_hash       TEXT,               -- hash of the definition span when the note was last synced
  stale           INTEGER NOT NULL DEFAULT 0,
  synced_at       TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS kg_note_file(
  notes_path TEXT PRIMARY KEY, mtime_ns INTEGER NOT NULL, content_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS kg_note_problem(          -- unmatched / ambiguous / lint findings from the last sync
  notes_path TEXT, line INTEGER, heading TEXT, kind TEXT, message TEXT);
```
`meta.kg_notes_dir` records the notes directory (set by `kg index --notes-dir`,
`kg notes sync --notes-dir`, or defaulted to `<kg_repo_root>/notes`).

Body hash: FNV-1a-64 over whitespace-normalized text of the symbol's
definition span (SCOPE location; TOKEN line if no SCOPE) read from
`filecontent`. Empty when the symbol has no indexed location.

Staleness anchor lives **in the markdown**, not only in the cache: the
optional metadata comment right under a block's heading
(`<!-- kg-hash: <fnv1a64 of the definition body>  kg-commit: <sha> -->`)
records the body the note was written against. `scaffold` writes it,
`kg notes accept SYMBOL|--all` rewrites it to the current body hash (the only
markdown edit kg ever makes besides scaffolding), and `sync`/`kg update`
compute `stale = (current body hash != kg-hash in the markdown)`. Because the
anchor is versioned with the note, staleness is detectable after a cache
rebuild (`kg index --force` + `sync`) and in any other worktree. Blocks
without a `kg-hash` are never reported stale (nothing to compare against);
`notes status --unanchored` lists them. The cache keeps a copy of both hashes
for fast queries.

## Matching & sync problem kinds

`kg::notes::matchBlock()` resolves a `##` heading against the DB exactly
like `kg query`'s own symbol resolution, then narrows by `### signature`
when there's more than one candidate. `kg notes sync`/`status --problems`
report, per block or per file:

- `unmatched` -- zero symbols resolve the heading.
- `ambiguous` -- more than one candidate remains after signature narrowing;
  `message` lists every candidate's id + signature.
- `duplicate-target` -- a later block resolves to a symbol an earlier block
  (file-then-line order) already claimed; parsed but never persisted.
- `signature-mismatch` -- exactly one candidate resolves the heading (no
  narrowing needed), but the block's non-empty `### signature` matches
  neither the candidate's full signature nor its parameter-list suffix. The
  block still matches (`sync` writes it to `kg_note` as usual) -- this is a
  warning that the written signature is stale, not a matching failure.
- `orphaned-file` -- a notes file's own source path (`notes_path` minus the
  trailing `.md`, resolved against `meta.kg_repo_root`) no longer exists as
  an indexed `file` row (e.g. after `git mv`ing the source without moving
  its notes alongside it). One problem per file, independent of whether
  individual blocks in it still match by name.

## CLI

```
kg notes scaffold -d DB [--notes-dir DIR] (--all | --file SRC-GLOB | --class QNAME | SYMBOL)
                  [--kind FUNCTION|METHOD]... [--dry-run] [--json]
kg notes sync     -d DB [--notes-dir DIR] [--strict] [--json]         # exit 0; 3 with --strict if problems
kg notes lint     --notes-dir DIR [--strict] [--json]                 # no DB
kg notes fmt      --notes-dir DIR [--check] [--json]                  # no DB; canonical re-render
kg notes show     -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]
kg notes status   -d DB [--stale] [--unfilled] [--problems] [--json]
kg notes accept   -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]
kg query context  -d DB SYMBOL [--depth 1] [--no-notes] [--notes-full] [--json]  # seed + callers + callees (+ overrides/bases one hop), notes embedded
kg dbdiff A B     [--only CATEGORY]...                                 # restrict the diff to just the given category name(s), e.g. --only note
kg index ... [--notes-dir DIR]        # records meta.kg_notes_dir (own meta key, not part of kg_options_json)
kg update ...                          # runs sync + staleness at the end
```
JSON schemas: `kg.notes-sync/1`, `kg.note/1`, `kg.notes-status/1`,
`kg.notes-lint/1`, `kg.notes-scaffold/1`, `kg.notes-accept/1`,
`kg.notes-fmt/1`; `kg.update-summary/1` carries a `notes{noted, stale,
newly_stale, problems}` object; `context` reuses `kg.graph/1` with an
optional `notes` object on each node (`{brief, what, how, signature, pre[],
post[], bugs[], suggestions[], path, line, filled, stale}` or `null`). See
`docs/JSON_SCHEMA.md` for the exact shapes.

Scaffold is **idempotent and non-destructive**: existing blocks are never
rewritten; a symbol whose target file already has a matching block (via
`matchBlock()`) is skipped ("skipped-existing"); everything else is
appended, in definition-line order, after the file's unchanged existing
bytes -- so an unaffected file isn't even re-written (byte-for-byte no-op on
a repeat run with the same selection). `accept` rewrites ONLY the
`kg-hash`/`kg-commit` comment (refusing a file that isn't already in
`fmt`'s canonical form); `fmt` is the plain `parseNotesFile` ->
`renderFile()` re-render, with `--check` for CI use.

`kg index --notes-dir DIR` records `meta.kg_notes_dir` (absolute) as its own
meta key -- deliberately NOT folded into `kg_options_json`, so changing it
never counts as an "option change" for `kg update`'s stored-options
comparison. When omitted, notes-aware verbs (and `kg update`'s own
post-update sync) fall back to `<kg_repo_root>/notes` if that directory
exists. At the end of a successful `kg update`, if a notes dir is known and
exists, `kg::notes::syncNotes()` runs against the freshly re-indexed working
copy (report-only: a notes-dir problem or sync exception never fails the
update) and the summary/stderr report which notes went newly stale.

## Test data

`scripts/kg-notes-fake.py --symbols-json FILE --notes-dir DIR --seed N
[--fill-ratio R]`: generates a notes tree from a `kg notes symbols --json`
(or bare-array) symbol list, filling placeholders with deterministic
pseudo-random text (seeded) for a fraction of blocks/sections; used by
Phase 1/2 tests, and to inject synthetic overloads/stale/unanchored blocks
(`--overloads`/`--stale-ratio`/`--drop-hash-ratio`) no real corpus would
reliably have.

`scripts/kg-notes-fake.py --fill-existing DIR --seed N [--fill-ratio R]`
(Phase 3): the realistic path -- parses an ALREADY-scaffolded (real `kg
notes scaffold --all`, real symbols, real `kg-hash`) notes tree in place and
replaces `_TODO_` placeholders with the same deterministic lorem text,
preserving headings, signatures and `kg-hash`/`kg-commit` comments
byte-for-byte. `scaffold --all` → `--fill-existing` → `sync` is the intended
realistic test/demo sequence.

## Phases

1. **Parser + model + lint + fake generator** (`kg_notes` lib: `Markdown.{h,cpp}`,
   `NoteModel.h`, `Lint`; `kg notes lint`; `scripts/kg-notes-fake.py` scaffold
   part depends on phase 3, so the script initially generates from a symbol
   list produced by `kg query`… see phase 1 brief). Unit tests: goldens,
   overload disambiguation, fences, CRLF, malformed input, fixed point.
2. **Cache + sync + show + status** (`kg_note*` tables, `NotesStore`, matching by
   qualified name + signature, body hash, filled/stale, problems report,
   `--json` schemas, in-memory overlay for stale cache). Fixture tests.
3. **Scaffold + `kg update` integration + accept** — DONE. `kg notes
   scaffold` (`--all`/`--file`/`--class`/`SYMBOL`, idempotent/non-destructive,
   shares symbol selection with `kg notes symbols` via
   `kg::notes::listSymbols()`); `kg notes accept` (re-anchors `kg-hash`/
   `kg-commit`, refusing non-canonical files); `kg notes fmt` (canonical
   re-render, `--check` for CI); `signature-mismatch` matching warning and
   `orphaned-file` sync problem; `kg index --notes-dir` (own
   `meta.kg_notes_dir` key); `kg update`'s post-update notes sync (report-only,
   `notes{noted,stale,newly_stale,problems}` in `kg.update-summary/1`);
   `scripts/kg-notes-fake.py --fill-existing`. Scenario test steps (edit
   function body → stale; edit an unrelated function → nothing new stale;
   accept → clears, byte-identical except the hash; `git mv` → orphaned-file;
   delete function → unmatched) in `test/update/update_scenario.cmake`; unit
   coverage in `test/unit/test_notes_scaffold.cpp`.
4. **`kg query context` + docs + proofs -- DONE.** `kg query context SYMBOL
   [--depth N] [--no-notes] [--notes-full]` reuses `kg.graph/1` exactly
   (seed + `callers`/`callees` union to `--depth`, one-hop
   `overrides`/`overridden-by`, and -- for a CLASS/STRUCT seed -- one-hop
   `bases`/`derived`), with every node object gaining a `"notes"` field
   (the `kg.note/1` note object, or `null`; `--no-notes` omits the key
   entirely). Text mode prints an indented `brief:` line (tagged
   `[stale]`/`[unfilled]`) under any noted node; `--notes-full` prints the
   whole rendered block instead. Implemented in `kg_out`
   (`JsonWriter::graph()`'s new `notesBySerializedName` map parameter,
   `TextWriter::writeBfsResult()`'s new `NodeNoteText` map parameter) and
   the CLI (which builds those maps from `kg::notes::openNotes()`) --
   `kg_out` still never links `kg_notes`, keeping the dependency graph
   acyclic (`cli -> {notes, out, query}`). `kg dbdiff` gained a
   `note`/`note_problem` category (keyed by `serialized_name`, never
   `node.id` or a path -- see the cache schema's own stability principle)
   and a `--only CATEGORY` flag to restrict a diff to exactly one category,
   used by `test/update/notes_cross_worktree.cmake`'s CTest
   (`notes_cross_worktree`, label `update`): two independently-`kg index`d
   clones of the same git history (a real `git clone` to a second
   worktree), each syncing notes against their own `notes/` directory
   (arrived via the clone, never re-scaffolded in the second worktree),
   end up with an IDENTICAL `note` category via `kg dbdiff --only note`
   even though a plain `kg dbdiff` (node/edge/location rows, which embed
   each worktree's own absolute paths) is very much not equal -- proving
   notes travel with git, independent of any one `.srctrldb`. Real-corpus
   coverage in `test/update/cucs_update_scenario.cmake`'s
   `cucs_update_integration` (scaffolds/fills/syncs notes for
   `cucascade::memory::reservation_limit_policy` and its three derived
   policies on the real cucs corpus, asserts `kg notes sync` stays well
   under 2s and that `kg query context`'s JSON carries `notes` for both a
   seed and its override chain). See `docs/KNOWN_LIMITATIONS.md`'s "Notes"
   section for what this does NOT detect (transitive staleness through a
   noted function's callees; TU-local symbol identity across worktrees at
   different absolute paths).
