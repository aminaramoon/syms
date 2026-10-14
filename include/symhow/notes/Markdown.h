#pragma once

#include <string>
#include <string_view>

#include "symhow/notes/NoteModel.h"

// Fence-aware markdown parser/renderer for `symhow notes` files (Phase 1 -- see
// docs/NOTES_DESIGN.md's "Markdown format" section, which this implements
// exactly). No DB dependency.
namespace symhow::notes
{

// Parses `content` (the full text of one `<notes-dir>/<source-path>.md`
// file) into a NotesFile. `path` is stored verbatim as NotesFile::path (not
// resolved/read from disk -- callers that have a filesystem path read it
// themselves and pass both the bytes and the path in).
//
// The parser never throws: malformed input produces NotesFile::problems
// entries instead. CRLF line endings are tolerated (normalized away).
// Section order in the file is NOT enforced (order-insensitive parse); only
// renderBlock()/renderFile() below impose canonical order.
//
// When `strict` is true, every section a block doesn't have (what/how/
// signature/pre/post/bugs/suggestions) adds a "missing-section" Problem.
NotesFile parseNotesFile(std::string_view content, std::string path, bool strict = false);

// Renders one block in the canonical format documented in
// docs/NOTES_DESIGN.md (heading, brief, `### what ?`, `### how ?`,
// `### signature` fenced with ```cpp, `### condition` with its four `####`
// subsections). Unfilled sections render as the `_TODO_` placeholder.
// parseNotesFile(renderBlock(b)) reproduces b's content (modulo line
// numbers, which shift to match the rendered text).
std::string renderBlock(const NoteBlock& block);

// Renders a full file: "# <fileHeading>\n\n" followed by every block from
// renderBlock(), each pair of blocks separated by a single blank line.
std::string renderFile(const NotesFile& file);

// Collapses runs of whitespace (including newlines) to a single space,
// trims leading/trailing whitespace, and strips one trailing ';' (with any
// whitespace before it), if present. Used to normalize `### signature`
// fence contents so two written-out spacings of the same signature compare
// equal, and to compare/dedupe overload signatures.
std::string normalizeSignature(std::string_view signature);

// Builds a scaffold block for `qualifiedName`/`signature` with every
// free-text/list section left as the `_TODO_` placeholder -- the canonical
// "unfilled" block. Used by Phase 3's `symhow notes scaffold` and by
// scripts/symhow-notes-fake.py's placeholder blocks (kept byte-for-byte in sync
// with this function; see test_notes_markdown.cpp's
// "placeholderBlockMarkdown parses to an unfilled block" case).
//
// When `bodyHash` and/or `commit` is non-empty, emits the
// `<!-- symhow-hash: ... symhow-commit: ... -->` staleness-anchor comment right
// after the heading (see NoteBlock::bodyHash's comment).
std::string placeholderBlockMarkdown(
    const std::string& qualifiedName, const std::string& signature, const std::string& bodyHash = "", const std::string& commit = "");

}  // namespace symhow::notes
