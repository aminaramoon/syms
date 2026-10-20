//! Port of test/unit/test_notes_markdown.cpp (pure markdown-layer cases; no
//! DB, no CLI). The final C++ TEST_CASE ("fake generator output lints as
//! expected and round-trips") shells out to the `symhow` binary -- see the
//! TODO(parity) note at the bottom of this file.

use symhow::notes::{
    normalize_signature, parse_notes_file, placeholder_block_markdown, render_block, render_file,
    NoteBlock, Problem, TextSection, PLACEHOLDER,
};

fn has_problem(problems: &[Problem], kind: &str, line: i32) -> bool {
    problems.iter().any(|p| p.kind == kind && p.line == line)
}

fn count_problems(problems: &[Problem], kind: &str) -> usize {
    problems.iter().filter(|p| p.kind == kind).count()
}

// -----------------------------------------------------------------------
// Golden example (docs/NOTES_DESIGN.md's "Markdown format" section).
// -----------------------------------------------------------------------

const GOLDEN: &str = "# src/planner/query.cpp\n\
\n\
## sirius::planner::query::get_pipeline\n\
One-line brief. (free text until the first `###`)\n\
### what ?\n\
_TODO_\n\
### how ?\n\
_TODO_\n\
### signature\n\
```cpp\n\
sirius::pipeline::sirius_pipeline *get_pipeline(op::sirius_physical_operator *)\n\
```\n\
### condition\n\
#### pre-condition\n\
1. _TODO_\n\
#### post-condition\n\
1. _TODO_\n\
#### bugs\n\
1. _TODO_\n\
#### suggestions\n\
1. _TODO_\n";

#[test]
fn parse_notes_file_golden_example_from_notes_design() {
    let f = parse_notes_file(GOLDEN, "src/planner/query.cpp.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.file_heading, "src/planner/query.cpp");
    assert_eq!(f.blocks.len(), 1);

    let b = &f.blocks[0];
    assert_eq!(b.heading, "sirius::planner::query::get_pipeline");
    assert_eq!(b.heading_line, 3);
    assert_eq!(b.brief, "One-line brief. (free text until the first `###`)");
    assert!(b.what.present);
    assert!(b.what.text.is_empty());
    assert!(b.how.present);
    assert!(b.how.text.is_empty());
    assert!(b.signature.present);
    assert_eq!(
        b.signature.text,
        "sirius::pipeline::sirius_pipeline *get_pipeline(op::sirius_physical_operator *)"
    );
    assert!(b.pre.present);
    assert!(b.pre.items.is_empty());
    assert!(b.post.present);
    assert!(b.post.items.is_empty());
    assert!(b.bugs.present);
    assert!(b.bugs.items.is_empty());
    assert!(b.suggestions.present);
    assert!(b.suggestions.items.is_empty());
    assert!(b.filled()); // brief is non-empty
    assert!(b.body_hash.is_empty());
    assert!(b.commit.is_empty());
    assert_eq!(b.meta_line, 0);
}

#[test]
fn render_parse_fixed_point_over_the_golden_example() {
    let f = parse_notes_file(GOLDEN, "p.md", false);
    let rendered = render_file(&f);
    let f2 = parse_notes_file(&rendered, "p.md", false);
    assert!(f2.problems.is_empty());
    assert_eq!(f2.blocks.len(), f.blocks.len());
    assert_eq!(f2.file_heading, f.file_heading);
    assert_eq!(f2.blocks[0].heading, f.blocks[0].heading);
    assert_eq!(f2.blocks[0].brief, f.blocks[0].brief);
    assert_eq!(f2.blocks[0].what.text, f.blocks[0].what.text);
    assert_eq!(f2.blocks[0].how.text, f.blocks[0].how.text);
    assert_eq!(f2.blocks[0].signature.text, f.blocks[0].signature.text);
    assert_eq!(f2.blocks[0].pre.items, f.blocks[0].pre.items);
    assert_eq!(f2.blocks[0].post.items, f.blocks[0].post.items);
    assert_eq!(f2.blocks[0].bugs.items, f.blocks[0].bugs.items);
    assert_eq!(f2.blocks[0].suggestions.items, f.blocks[0].suggestions.items);

    // Idempotence: re-rendering f2 reproduces exactly `rendered`.
    assert_eq!(render_file(&f2), rendered);
}

// -----------------------------------------------------------------------
// Overloads
// -----------------------------------------------------------------------

#[test]
fn overloads_two_blocks_same_heading_different_signatures() {
    let content = "# f.cpp\n\
\n\
## ns::foo\n\
brief a\n\
### signature\n\
```cpp\n\
int foo(int)\n\
```\n\
\n\
## ns::foo\n\
brief b\n\
### signature\n\
```cpp\n\
int foo(double)\n\
```\n";
    let f = parse_notes_file(content, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 2);
    assert_eq!(f.blocks[0].heading, "ns::foo");
    assert_eq!(f.blocks[1].heading, "ns::foo");
    assert_eq!(f.blocks[0].signature.text, "int foo(int)");
    assert_eq!(f.blocks[1].signature.text, "int foo(double)");
    assert_eq!(count_problems(&f.problems, "duplicate-block"), 0);
}

#[test]
fn include_inside_a_signature_fence_is_not_treated_as_a_heading() {
    let content = "# f.cpp\n\
\n\
## ns::foo\n\
brief\n\
### signature\n\
```cpp\n\
#include <vector>\n\
int foo()\n\
```\n";
    let f = parse_notes_file(content, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].signature.text, "#include <vector> int foo()");
}

#[test]
fn crlf_input_parses_identically_to_lf() {
    let mut out = String::new();
    for c in GOLDEN.chars() {
        if c == '\n' {
            out.push('\r');
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    let f = parse_notes_file(&out, "p.md", false);
    let f_lf = parse_notes_file(GOLDEN, "p.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), f_lf.blocks.len());
    assert_eq!(f.blocks[0].heading, f_lf.blocks[0].heading);
    assert_eq!(f.blocks[0].brief, f_lf.blocks[0].brief);
    assert_eq!(f.blocks[0].signature.text, f_lf.blocks[0].signature.text);
}

// -----------------------------------------------------------------------
// _TODO_ handling / list continuation
// -----------------------------------------------------------------------

#[test]
fn todo_only_block_is_unfilled_and_todo_list_items_are_dropped() {
    let content = "# f.cpp\n\
\n\
## ns::foo\n\
_TODO_\n\
### what ?\n\
_TODO_\n\
### condition\n\
#### pre-condition\n\
1. _TODO_\n\
2. real item\n\
3. _TODO_\n";
    let f = parse_notes_file(content, "f.md", false);
    assert_eq!(f.blocks.len(), 1);
    let b = &f.blocks[0];
    assert!(b.brief.is_empty());
    assert!(b.what.text.is_empty());
    assert_eq!(b.pre.items.len(), 1);
    assert_eq!(b.pre.items[0], "real item");
    assert!(b.filled()); // the surviving "real item" makes the block filled
}

#[test]
fn list_continuation_lines_are_appended_to_the_previous_item() {
    let content = "# f.cpp\n\
\n\
## ns::foo\n\
brief\n\
### condition\n\
#### bugs\n\
1. first line\n\
   continues here\n\
2. second item\n";
    let f = parse_notes_file(content, "f.md", false);
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].bugs.items.len(), 2);
    assert_eq!(f.blocks[0].bugs.items[0], "first line continues here");
    assert_eq!(f.blocks[0].bugs.items[1], "second item");
}

// -----------------------------------------------------------------------
// Every Problem kind
// -----------------------------------------------------------------------

#[test]
fn problem_unknown_heading_bad_section_name() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n### bogus\ntext\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "unknown-heading", 5));
}

#[test]
fn problem_unknown_heading_subsection_outside_condition() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n### what ?\nx\n#### pre-condition\n1. y\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "unknown-heading", 7));
}

#[test]
fn problem_duplicate_section() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n### what ?\none\n### what ?\ntwo\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "duplicate-section", 7));
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].what.text, "one"); // first occurrence wins
}

#[test]
fn problem_duplicate_block_same_heading_same_signature() {
    let c = "# f.cpp\n\
\n\
## ns::foo\n\
brief\n\
### signature\n\
```cpp\n\
int foo()\n\
```\n\
\n\
## ns::foo\n\
brief2\n\
### signature\n\
```cpp\n\
int foo()\n\
```\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "duplicate-block", 10));
}

#[test]
fn no_duplicate_block_when_both_signatures_are_empty() {
    // (ambiguous, not duplicate)
    let c = "# f.cpp\n\n## ns::foo\nbrief\n\n## ns::foo\nbrief2\n";
    let f = parse_notes_file(c, "f.md", false);
    assert_eq!(count_problems(&f.problems, "duplicate-block"), 0);
}

#[test]
fn problem_unclosed_fence() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n### signature\n```cpp\nint foo()\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "unclosed-fence", 6));
}

#[test]
fn problem_missing_signature_fence() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n### signature\n_TODO_\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "missing-signature-fence", 5));
}

#[test]
fn problem_empty_heading() {
    // "## " with a trailing space and nothing else
    let c = "# f.cpp\n\n## \nbrief\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "empty-heading", 3));
    assert_eq!(f.blocks.len(), 1);
    assert!(f.blocks[0].heading.is_empty());
}

#[test]
fn problem_text_outside_block() {
    let c = "stray text\n# f.cpp\n\n## ns::foo\nbrief\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "text-outside-block", 1));
}

#[test]
fn problem_missing_section_only_reported_in_strict_mode() {
    let c = "# f.cpp\n\n## ns::foo\nbrief\n";
    let lax = parse_notes_file(c, "f.md", false);
    assert_eq!(count_problems(&lax.problems, "missing-section"), 0);

    let strict = parse_notes_file(c, "f.md", true);
    assert_eq!(count_problems(&strict.problems, "missing-section"), 7);
    assert!(has_problem(&strict.problems, "missing-section", 3));
}

#[test]
fn problem_stray_heading_level() {
    let c = "# f.cpp\n# again\n\n## ns::foo\nbrief\n##### deep\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "stray-heading-level", 2));
    assert!(has_problem(&f.problems, "stray-heading-level", 6));
}

#[test]
fn problem_misplaced_meta() {
    let c = "# f.cpp\n\n## ns::foo\nbrief text here\n<!-- symhow-hash: abc123 -->\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(has_problem(&f.problems, "misplaced-meta", 5));
    assert_eq!(f.blocks.len(), 1);
    assert!(f.blocks[0].body_hash.is_empty());
}

// -----------------------------------------------------------------------
// symhow-hash / symhow-commit staleness anchor comment
// -----------------------------------------------------------------------

#[test]
fn meta_comment_immediately_after_heading_is_parsed_into_body_hash_and_commit() {
    let c = "# f.cpp\n\n## ns::foo\n<!-- symhow-hash: abc123  symhow-commit: deadbeef -->\nbrief text\n";
    let f = parse_notes_file(c, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].body_hash, "abc123");
    assert_eq!(f.blocks[0].commit, "deadbeef");
    assert_eq!(f.blocks[0].meta_line, 4);
    assert_eq!(f.blocks[0].brief, "brief text");
}

#[test]
fn meta_comment_absent_leaves_body_hash_and_commit_empty() {
    let c = "# f.cpp\n\n## ns::foo\nbrief text\n";
    let f = parse_notes_file(c, "f.md", false);
    assert_eq!(f.blocks.len(), 1);
    assert!(f.blocks[0].body_hash.is_empty());
    assert!(f.blocks[0].commit.is_empty());
    assert_eq!(f.blocks[0].meta_line, 0);
}

#[test]
fn meta_comment_round_trips_through_render_parse() {
    let b = NoteBlock {
        heading: "ns::foo".to_string(),
        body_hash: "0123456789abcdef".to_string(),
        commit: "cafefeed".to_string(),
        signature: TextSection { present: true, text: "int foo()".to_string(), ..Default::default() },
        ..Default::default()
    };
    let rendered = render_block(&b);
    assert_eq!(&rendered[..rendered.find('\n').unwrap()], "## ns::foo");
    let f = parse_notes_file(&rendered, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].body_hash, b.body_hash);
    assert_eq!(f.blocks[0].commit, b.commit);
    assert_eq!(f.blocks[0].meta_line, 2);
}

// -----------------------------------------------------------------------
// normalizeSignature
// -----------------------------------------------------------------------

#[test]
fn normalize_signature_cases() {
    assert_eq!(normalize_signature("foo()"), "foo()");
    assert_eq!(normalize_signature("foo();"), "foo()");
    assert_eq!(normalize_signature("  int   f( int x ) ;  "), "int f( int x )");
    assert_eq!(normalize_signature("a\n b\tc"), "a b c");
    assert_eq!(normalize_signature(""), "");
    assert_eq!(normalize_signature(";"), "");
    assert_eq!(PLACEHOLDER, "_TODO_");
}

// -----------------------------------------------------------------------
// placeholderBlockMarkdown
// -----------------------------------------------------------------------

#[test]
fn placeholder_block_markdown_parses_to_an_unfilled_block_with_given_heading_and_signature() {
    let block = placeholder_block_markdown("ns::Foo::bar", "int bar ( int ) const ;", "", "");
    let f = parse_notes_file(&block, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].heading, "ns::Foo::bar");
    assert!(!f.blocks[0].filled());
    assert_eq!(f.blocks[0].signature.text, normalize_signature("int bar ( int ) const ;"));
    assert!(f.blocks[0].body_hash.is_empty());
}

#[test]
fn placeholder_block_markdown_with_body_hash_and_commit_emits_and_round_trips_the_meta_comment() {
    let block = placeholder_block_markdown("ns::Foo::bar", "int bar()", "deadbeef01234567", "abc123");
    let f = parse_notes_file(&block, "f.md", false);
    assert!(f.problems.is_empty());
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].body_hash, "deadbeef01234567");
    assert_eq!(f.blocks[0].commit, "abc123");
}

// -----------------------------------------------------------------------
// Fake generator: run it, then lint the result (Phase 1 verification).
// -----------------------------------------------------------------------

// TODO(parity): needs CLI -- the C++ "fake generator output lints as
// expected and round-trips" case runs scripts/symhow-notes-fake.py and then
// shells out to the `symhow` binary for `notes lint` / `notes lint --strict
// --json` (schema "symhow.notes-lint/1", exit code 3 under --strict). The
// `symhow` CLI is not ported yet; the library half of that case (parse ->
// renderFile -> byte-identical) is covered by
// render_parse_fixed_point_over_the_golden_example and by the generator
// round-trips exercised in tests/notes_sync.rs and tests/notes_scaffold.rs.
