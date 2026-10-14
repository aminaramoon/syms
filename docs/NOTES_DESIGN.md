# symhow notes — function-level knowledge attached to the symbol graph

## Goal

Let agents (and humans) attach durable, reviewable knowledge to functions —
*brief / what / how / signature / pre-conditions / post-conditions / bugs /
suggestions* — and retrieve it together with a function's callers and callees
in one call, so code does not have to be re-read in every session/worktree.

Design principles:

1. **Markdown in git is the source of truth.** Notes live under the repo
   (`<repo>/notes/...`), are versioned, diffable, reviewable, and shared across
   worktrees/sessions. The `.srctrldb` only holds a **derived cache**
   (`symhow_note*` tables) that `symhow notes sync` rebuilds from the markdown.
2. **Keyed by stable identity, never by `node.id`.** Node ids change whenever a
   file is re-indexed. Notes are keyed by the symbol's `serialized_name`
   (qualified name + signature), which is also the writer's own dedup key.
3. **Querying never writes the DB.** `symhow query`/`symhow notes show` detect a stale
   cache (a notes file changed on disk since the last sync) and overlay the
   re-parsed file **in memory**, printing a hint to run `symhow notes sync`.
4. **Staleness is tracked automatically.** Each note stores a hash of its
   function's definition body (computed from `filecontent` + the SCOPE
   location symhow already records). `symhow update` recomputes hashes for noted
   symbols in re-indexed files and flags notes whose body changed.

## Markdown format (one file per source file)

Path: `<notes-dir>/<source path relative to repo root>.md`
(e.g. `notes/src/planner/query.cpp.md`). A function's note lives in the file
of its **definition** (SCOPE location; falls back to the declaration file).

```markdown
# src/planner/query.cpp

## sirius::planner::query::get_pipeline
<!-- symhow-hash: 9f7553b7a8b8475637bf57fc0844705a  symhow-commit: 0dca58628 -->
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
  **qualified name** (symhow `qualified_name`, i.e. `namePath()`, no parameter list).
- `### signature` holds the symhow `signature()` string in a ```` ```cpp ```` fence;
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

## Cache schema (symhow-private tables; extra tables are harmless to Sourcetrail)

```sql
CREATE TABLE IF NOT EXISTS symhow_note(
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
CREATE TABLE IF NOT EXISTS symhow_note_file(
  notes_path TEXT PRIMARY KEY, mtime_ns INTEGER NOT NULL, content_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS symhow_note_problem(          -- unmatched / ambiguous / lint findings from the last sync
  notes_path TEXT, line INTEGER, heading TEXT, kind TEXT, message TEXT);
```
`meta.symhow_notes_dir` records the notes directory (set by `symhow index --notes-dir`,
`symhow notes sync --notes-dir`, or defaulted to `<symhow_repo_root>/notes`).

Body hash: FNV-1a-64 over whitespace-normalized text of the symbol's
definition span (SCOPE location; TOKEN line if no SCOPE) read from
`filecontent`. Empty when the symbol has no indexed location.

Staleness anchor lives **in the markdown**, not only in the cache: the
optional metadata comment right under a block's heading
(`<!-- symhow-hash: <fnv1a64 of the definition body>  symhow-commit: <sha> -->`)
records the body the note was written against. `scaffold` writes it,
`symhow notes accept SYMBOL|--all` rewrites it to the current body hash (the only
markdown edit symhow ever makes besides scaffolding), and `sync`/`symhow update`
compute `stale = (current body hash != symhow-hash in the markdown)`. Because the
anchor is versioned with the note, staleness is detectable after a cache
rebuild (`symhow index --force` + `sync`) and in any other worktree. Blocks
without a `symhow-hash` are never reported stale (nothing to compare against);
`notes status --unanchored` lists them. The cache keeps a copy of both hashes
for fast queries.

## Matching & sync problem kinds

`symhow::notes::matchBlock()` resolves a `##` heading against the DB exactly
like `symhow query`'s own symbol resolution, then narrows by `### signature`
when there's more than one candidate. `symhow notes sync`/`status --problems`
report, per block or per file:

- `unmatched` -- zero symbols resolve the heading.
- `ambiguous` -- more than one candidate remains after signature narrowing;
  `message` lists every candidate's id + signature.
- `duplicate-target` -- a later block resolves to a symbol an earlier block
  (file-then-line order) already claimed; parsed but never persisted.
- `signature-mismatch` -- exactly one candidate resolves the heading (no
  narrowing needed), but the block's non-empty `### signature` matches
  neither the candidate's full signature nor its parameter-list suffix. The
  block still matches (`sync` writes it to `symhow_note` as usual) -- this is a
  warning that the written signature is stale, not a matching failure.
- `orphaned-file` -- a notes file's own source path (`notes_path` minus the
  trailing `.md`, resolved against `meta.symhow_repo_root`) no longer exists as
  an indexed `file` row (e.g. after `git mv`ing the source without moving
  its notes alongside it). One problem per file, independent of whether
  individual blocks in it still match by name.

## CLI

```
symhow notes scaffold -d DB [--notes-dir DIR] (--all | --file SRC-GLOB | --class QNAME | SYMBOL)
                  [--kind FUNCTION|METHOD]... [--dry-run] [--json]
symhow notes sync     -d DB [--notes-dir DIR] [--strict] [--json]         # exit 0; 3 with --strict if problems
symhow notes lint     --notes-dir DIR [--strict] [--json]                 # no DB
symhow notes fmt      --notes-dir DIR [--check] [--json]                  # no DB; canonical re-render
symhow notes show     -d DB SYMBOL [--section brief|what|how|signature|pre|post|bugs|suggestions] [--json]
symhow notes status   -d DB [--stale] [--unfilled] [--problems] [--json]
symhow notes accept   -d DB (SYMBOL | --all | --file SRC-GLOB) [--json]
symhow query context  -d DB SYMBOL [--depth 1] [--no-notes] [--notes-full] [--json]  # seed + callers + callees (+ overrides/bases one hop), notes embedded
symhow dbdiff A B     [--only CATEGORY]...                                 # restrict the diff to just the given category name(s), e.g. --only note
symhow index ... [--notes-dir DIR]        # records meta.symhow_notes_dir (own meta key, not part of symhow_options_json)
symhow update ...                          # runs sync + staleness at the end
```
JSON schemas: `symhow.notes-sync/1`, `symhow.note/1`, `symhow.notes-status/1`,
`symhow.notes-lint/1`, `symhow.notes-scaffold/1`, `symhow.notes-accept/1`,
`symhow.notes-fmt/1`; `symhow.update-summary/1` carries a `notes{noted, stale,
newly_stale, problems}` object; `context` reuses `symhow.graph/1` with an
optional `notes` object on each node (`{brief, what, how, signature, pre[],
post[], bugs[], suggestions[], path, line, filled, stale}` or `null`). See
`docs/JSON_SCHEMA.md` for the exact shapes.

Scaffold is **idempotent and non-destructive**: existing blocks are never
rewritten; a symbol whose target file already has a matching block (via
`matchBlock()`) is skipped ("skipped-existing"); everything else is
appended, in definition-line order, after the file's unchanged existing
bytes -- so an unaffected file isn't even re-written (byte-for-byte no-op on
a repeat run with the same selection). `accept` rewrites ONLY the
`symhow-hash`/`symhow-commit` comment (refusing a file that isn't already in
`fmt`'s canonical form); `fmt` is the plain `parseNotesFile` ->
`renderFile()` re-render, with `--check` for CI use.

`symhow index --notes-dir DIR` records `meta.symhow_notes_dir` (absolute) as its own
meta key -- deliberately NOT folded into `symhow_options_json`, so changing it
never counts as an "option change" for `symhow update`'s stored-options
comparison. When omitted, notes-aware verbs (and `symhow update`'s own
post-update sync) fall back to `<symhow_repo_root>/notes` if that directory
exists. At the end of a successful `symhow update`, if a notes dir is known and
exists, `symhow::notes::syncNotes()` runs against the freshly re-indexed working
copy (report-only: a notes-dir problem or sync exception never fails the
update) and the summary/stderr report which notes went newly stale.

## Test data

`scripts/symhow-notes-fake.py --symbols-json FILE --notes-dir DIR --seed N
[--fill-ratio R]`: generates a notes tree from a `symhow notes symbols --json`
(or bare-array) symbol list, filling placeholders with deterministic
pseudo-random text (seeded) for a fraction of blocks/sections; used by
Phase 1/2 tests, and to inject synthetic overloads/stale/unanchored blocks
(`--overloads`/`--stale-ratio`/`--drop-hash-ratio`) no real corpus would
reliably have.

`scripts/symhow-notes-fake.py --fill-existing DIR --seed N [--fill-ratio R]`
(Phase 3): the realistic path -- parses an ALREADY-scaffolded (real `symhow
notes scaffold --all`, real symbols, real `symhow-hash`) notes tree in place and
replaces `_TODO_` placeholders with the same deterministic lorem text,
preserving headings, signatures and `symhow-hash`/`symhow-commit` comments
byte-for-byte. `scaffold --all` → `--fill-existing` → `sync` is the intended
realistic test/demo sequence.

## Phases

1. **Parser + model + lint + fake generator** (`symhow_notes` lib: `Markdown.{h,cpp}`,
   `NoteModel.h`, `Lint`; `symhow notes lint`; `scripts/symhow-notes-fake.py` scaffold
   part depends on phase 3, so the script initially generates from a symbol
   list produced by `symhow query`… see phase 1 brief). Unit tests: goldens,
   overload disambiguation, fences, CRLF, malformed input, fixed point.
2. **Cache + sync + show + status** (`symhow_note*` tables, `NotesStore`, matching by
   qualified name + signature, body hash, filled/stale, problems report,
   `--json` schemas, in-memory overlay for stale cache). Fixture tests.
3. **Scaffold + `symhow update` integration + accept** — DONE. `symhow notes
   scaffold` (`--all`/`--file`/`--class`/`SYMBOL`, idempotent/non-destructive,
   shares symbol selection with `symhow notes symbols` via
   `symhow::notes::listSymbols()`); `symhow notes accept` (re-anchors `symhow-hash`/
   `symhow-commit`, refusing non-canonical files); `symhow notes fmt` (canonical
   re-render, `--check` for CI); `signature-mismatch` matching warning and
   `orphaned-file` sync problem; `symhow index --notes-dir` (own
   `meta.symhow_notes_dir` key); `symhow update`'s post-update notes sync (report-only,
   `notes{noted,stale,newly_stale,problems}` in `symhow.update-summary/1`);
   `scripts/symhow-notes-fake.py --fill-existing`. Scenario test steps (edit
   function body → stale; edit an unrelated function → nothing new stale;
   accept → clears, byte-identical except the hash; `git mv` → orphaned-file;
   delete function → unmatched) in `test/update/update_scenario.cmake`; unit
   coverage in `test/unit/test_notes_scaffold.cpp`.
4. **`symhow query context` + docs + proofs -- DONE.** `symhow query context SYMBOL
   [--depth N] [--no-notes] [--notes-full]` reuses `symhow.graph/1` exactly
   (seed + `callers`/`callees` union to `--depth`, one-hop
   `overrides`/`overridden-by`, and -- for a CLASS/STRUCT seed -- one-hop
   `bases`/`derived`), with every node object gaining a `"notes"` field
   (the `symhow.note/1` note object, or `null`; `--no-notes` omits the key
   entirely). Text mode prints an indented `brief:` line (tagged
   `[stale]`/`[unfilled]`) under any noted node; `--notes-full` prints the
   whole rendered block instead. Implemented in `symhow_out`
   (`JsonWriter::graph()`'s new `notesBySerializedName` map parameter,
   `TextWriter::writeBfsResult()`'s new `NodeNoteText` map parameter) and
   the CLI (which builds those maps from `symhow::notes::openNotes()`) --
   `symhow_out` still never links `symhow_notes`, keeping the dependency graph
   acyclic (`cli -> {notes, out, query}`). `symhow dbdiff` gained a
   `note`/`note_problem` category (keyed by `serialized_name`, never
   `node.id` or a path -- see the cache schema's own stability principle)
   and a `--only CATEGORY` flag to restrict a diff to exactly one category,
   used by `test/update/notes_cross_worktree.cmake`'s CTest
   (`notes_cross_worktree`, label `update`): two independently-`symhow index`d
   clones of the same git history (a real `git clone` to a second
   worktree), each syncing notes against their own `notes/` directory
   (arrived via the clone, never re-scaffolded in the second worktree),
   end up with an IDENTICAL `note` category via `symhow dbdiff --only note`
   even though a plain `symhow dbdiff` (node/edge/location rows, which embed
   each worktree's own absolute paths) is very much not equal -- proving
   notes travel with git, independent of any one `.srctrldb`. Real-corpus
   coverage in `test/update/cucs_update_scenario.cmake`'s
   `cucs_update_integration` (scaffolds/fills/syncs notes for
   `cucascade::memory::reservation_limit_policy` and its three derived
   policies on the real cucs corpus, asserts `symhow notes sync` stays well
   under 2s and that `symhow query context`'s JSON carries `notes` for both a
   seed and its override chain). See `docs/KNOWN_LIMITATIONS.md`'s "Notes"
   section for what this does NOT detect (transitive staleness through a
   noted function's callees; TU-local symbol identity across worktrees at
   different absolute paths).
