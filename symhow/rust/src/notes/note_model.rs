//! In-memory model for `symhow notes` markdown files (Phase 1 -- see
//! docs/NOTES_DESIGN.md). Pure data types; the parser/renderer that produces
//! and consumes them live in `markdown.rs`. No DB dependency in this phase.
//! Mirrors `symhow::notes::NoteModel.h`.

/// The placeholder token a scaffolded (unfilled) section/item is written as.
pub const PLACEHOLDER: &str = "_TODO_";

/// One lint/parse finding, always carrying the 1-based source line it
/// applies to (the offending heading's line, or the block's `##` line when
/// there is no more specific line -- see each kind's own callsite comment in
/// `markdown.rs`). `kind` is one of the exact strings documented in
/// docs/NOTES_DESIGN.md / the Phase 1 brief:
/// unknown-heading, duplicate-section, duplicate-block, unclosed-fence,
/// missing-signature-fence, empty-heading, text-outside-block,
/// missing-section, stray-heading-level, misplaced-meta.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Problem {
    pub line: i32,
    pub kind: String,
    pub message: String,
}

/// A free-text section (`### what ?`, `### how ?`, `### signature`).
/// `text` is trimmed; it is the empty string both when the section is
/// absent (`present == false`) and when it is present but its only content
/// is the `_TODO_` placeholder (`present == true`, `text == ""`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextSection {
    pub present: bool,
    pub line: i32, // line of the `###` heading, when present
    pub text: String,
}

/// A numbered/bulleted list section (`#### pre-condition`, etc). Numbering
/// markers are stripped; `_TODO_` items are dropped entirely (so an
/// all-placeholder list section has `present == true`, `items` empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSection {
    pub present: bool,
    pub line: i32, // line of the `####` heading, when present
    pub items: Vec<String>,
}

/// One `##` block: a single symbol's note. `heading` is the qualified name
/// exactly as written (trimmed); several blocks may share the same heading
/// when they document overloads, disambiguated by `signature`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteBlock {
    pub heading: String,
    pub heading_line: i32,

    /// Staleness anchor, carried in an optional HTML comment on the line(s)
    /// immediately after the `## heading` line, before the brief:
    /// `<!-- symhow-hash: <16-hex fnv1a64>  symhow-commit: <sha> -->`
    /// Both empty when the comment is absent. `meta_line` is the comment's
    /// own line (0 when absent). Phase 2's `sync` compares `body_hash`
    /// against the current body hash computed from the DB; Phase 3's
    /// `scaffold`/`accept` write this comment.
    pub body_hash: String,
    pub commit: String,
    pub meta_line: i32,

    pub brief: String,
    pub what: TextSection,
    pub how: TextSection,
    pub signature: TextSection,
    pub pre: ListSection,
    pub post: ListSection,
    pub bugs: ListSection,
    pub suggestions: ListSection,
}

impl NoteBlock {
    /// True if any of brief/what/how/pre/post/bugs/suggestions is non-empty.
    /// `signature` deliberately does NOT count -- a block that only carries
    /// its (correct, non-placeholder) signature is still "unfilled".
    pub fn filled(&self) -> bool {
        !self.brief.is_empty()
            || !self.what.text.is_empty()
            || !self.how.text.is_empty()
            || !self.pre.items.is_empty()
            || !self.post.items.is_empty()
            || !self.bugs.items.is_empty()
            || !self.suggestions.items.is_empty()
    }
}

/// One parsed `<notes-dir>/<source-path>.md` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesFile {
    pub path: String,        // as given to parse_notes_file(), not resolved/canonicalized
    pub file_heading: String, // text after the first `# `; may be empty
    pub blocks: Vec<NoteBlock>,
    pub problems: Vec<Problem>,
}
