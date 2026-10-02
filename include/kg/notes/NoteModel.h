#pragma once

#include <string>
#include <vector>

// In-memory model for `kg notes` markdown files (Phase 1 -- see
// docs/NOTES_DESIGN.md). Pure data types; the parser/renderer that produces
// and consumes them live in Markdown.h. No DB dependency in this phase.
namespace kg::notes
{

// The placeholder token a scaffolded (unfilled) section/item is written as.
inline constexpr const char* kPlaceholder = "_TODO_";

// One lint/parse finding, always carrying the 1-based source line it
// applies to (the offending heading's line, or the block's `##` line when
// there is no more specific line -- see each kind's own callsite comment in
// Markdown.cpp). `kind` is one of the exact strings documented in
// docs/NOTES_DESIGN.md / the Phase 1 brief:
//   unknown-heading, duplicate-section, duplicate-block, unclosed-fence,
//   missing-signature-fence, empty-heading, text-outside-block,
//   missing-section, stray-heading-level, misplaced-meta
struct Problem
{
    int line = 0;
    std::string kind;
    std::string message;
};

// A free-text section (`### what ?`, `### how ?`, `### signature`).
// `text` is trimmed; it is the empty string both when the section is
// absent (`present == false`) and when it is present but its only content
// is the `_TODO_` placeholder (`present == true`, `text == ""`).
struct TextSection
{
    bool present = false;
    int line = 0;  // line of the `###` heading, when present
    std::string text;
};

// A numbered/bulleted list section (`#### pre-condition`, etc). Numbering
// markers are stripped; `_TODO_` items are dropped entirely (so an
// all-placeholder list section has `present == true`, `items` empty).
struct ListSection
{
    bool present = false;
    int line = 0;  // line of the `####` heading, when present
    std::vector<std::string> items;
};

// One `##` block: a single symbol's note. `heading` is the qualified name
// exactly as written (trimmed); several blocks may share the same heading
// when they document overloads, disambiguated by `signature`.
struct NoteBlock
{
    std::string heading;
    int headingLine = 0;

    // Staleness anchor, carried in an optional HTML comment on the line(s)
    // immediately after the `## heading` line, before the brief:
    //   <!-- kg-hash: <16-hex fnv1a64>  kg-commit: <sha> -->
    // Both empty when the comment is absent. `metaLine` is the comment's
    // own line (0 when absent). Phase 2's `sync` compares `bodyHash`
    // against the current body hash computed from the DB; Phase 3's
    // `scaffold`/`accept` write this comment.
    std::string bodyHash;
    std::string commit;
    int metaLine = 0;

    std::string brief;
    TextSection what;
    TextSection how;
    TextSection signature;
    ListSection pre;
    ListSection post;
    ListSection bugs;
    ListSection suggestions;

    // True if any of brief/what/how/pre/post/bugs/suggestions is non-empty.
    // `signature` deliberately does NOT count -- a block that only carries
    // its (correct, non-placeholder) signature is still "unfilled".
    bool filled() const
    {
        return !brief.empty() || !what.text.empty() || !how.text.empty() || !pre.items.empty() || !post.items.empty()
               || !bugs.items.empty() || !suggestions.items.empty();
    }
};

// One parsed `<notes-dir>/<source-path>.md` file.
struct NotesFile
{
    std::string path;  // as given to parseNotesFile(), not resolved/canonicalized
    std::string fileHeading;  // text after the first `# `; may be empty
    std::vector<NoteBlock> blocks;
    std::vector<Problem> problems;
};

}  // namespace kg::notes
