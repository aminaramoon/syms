//! Fence-aware markdown parser/renderer for `symhow notes` files (Phase 1 --
//! see docs/NOTES_DESIGN.md's "Markdown format" section, which this implements
//! exactly). No DB dependency. Mirrors `symhow::notes::Markdown.cpp`.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use super::note_model::{ListSection, NoteBlock, NotesFile, Problem, TextSection, PLACEHOLDER};

// std::isspace in the C locale: space, \t, \n, \v, \f, \r (ASCII only).
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

fn trim(s: &str) -> String {
    s.trim_matches(is_space).to_string()
}

fn is_blank(s: &str) -> bool {
    trim(s).is_empty()
}

/// The ASCII whitespace character class used by the C++ regexes (std::regex
/// `\s` under the default locale is ASCII-only, unlike the `regex` crate's
/// Unicode-aware `\s`).
const WS: &str = r"[ \t\n\x0B\x0C\r]";

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(#+)(.*)$").unwrap())
}

fn item_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"^{}*(?:[0-9]+[.)]|[-*+]){}+(.*)$", WS, WS)).unwrap()
    })
}

fn symhow_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"^{}*<!--.*\bsymhow-(?:hash|commit)\b.*-->{}*$", WS, WS)).unwrap()
    })
}

fn kv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"([A-Za-z][A-Za-z0-9_-]*){}*:{}*([^{}]+)", WS, WS, WS)).unwrap()
    })
}

/// Splits `content` into lines (no trailing newline kept per line). CRLF and
/// lone-CR line endings are normalized to '\n' first.
fn split_lines(content: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            normalized.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else {
            normalized.push(c);
        }
    }
    if normalized.is_empty() {
        return Vec::new();
    }
    // getline() semantics: a trailing '\n' does not produce a final empty
    // line, but an inner "\n\n" does produce an empty line.
    let mut lines: Vec<String> = normalized.split('\n').map(|s| s.to_string()).collect();
    if normalized.ends_with('\n') {
        lines.pop();
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FenceRole {
    None,
    Open,
    Body,
    Close,
}

/// Pass A: fence-aware scan. Determines, for every line, whether it's inside
/// a ``` / ~~~ fence (Open/Body/Close) so the heading scanner below can skip
/// it entirely -- this is what makes `#include` inside a `### signature`
/// fence safe. Also collects "unclosed-fence" problems.
fn scan_fences(lines: &[String], problems: &mut Vec<Problem>) -> Vec<FenceRole> {
    let mut roles = vec![FenceRole::None; lines.len()];
    let mut in_fence = false;
    let mut fence_char = 0u8;
    let mut fence_len = 0usize;
    let mut fence_open_line: i32 = -1;

    for (i, line) in lines.iter().enumerate() {
        let t = trim(line);
        let t_bytes = t.as_bytes();
        if !in_fence {
            if t.len() >= 3 && (t_bytes[0] == b'`' || t_bytes[0] == b'~') {
                let c = t_bytes[0];
                let mut run_len = 0usize;
                while run_len < t_bytes.len() && t_bytes[run_len] == c {
                    run_len += 1;
                }
                if run_len >= 3 {
                    in_fence = true;
                    fence_char = c;
                    fence_len = run_len;
                    fence_open_line = i as i32;
                    roles[i] = FenceRole::Open;
                    continue;
                }
            }
            roles[i] = FenceRole::None;
        } else {
            // Closing fence: a run of >= fence_len of the same fence char and
            // nothing else (besides surrounding whitespace, already trimmed).
            let mut run_len = 0usize;
            while run_len < t_bytes.len() && t_bytes[run_len] == fence_char {
                run_len += 1;
            }
            let is_close = !t.is_empty() && run_len == t_bytes.len() && run_len >= fence_len;
            if is_close {
                roles[i] = FenceRole::Close;
                in_fence = false;
                fence_open_line = -1;
            } else {
                roles[i] = FenceRole::Body;
            }
        }
    }

    if in_fence && fence_open_line >= 0 {
        problems.push(Problem {
            line: fence_open_line + 1,
            kind: "unclosed-fence".to_string(),
            message: "fence opened here is never closed".to_string(),
        });
    }
    roles
}

/// Turns raw lines collected under a `####` list section into items:
/// numbering markers stripped, continuation lines appended to the previous
/// item, `_TODO_` items dropped.
fn parse_list_items(raw: &[String]) -> Vec<String> {
    let mut raw_items: Vec<String> = Vec::new();
    let mut have_item = false;
    for line in raw {
        if is_blank(line) {
            continue;
        }
        if let Some(caps) = item_re().captures(line) {
            raw_items.push(trim(&caps[1]));
            have_item = true;
        } else if have_item {
            let last = raw_items.last_mut().unwrap();
            last.push(' ');
            last.push_str(&trim(line));
        } else {
            raw_items.push(trim(line));
            have_item = true;
        }
    }
    raw_items.into_iter().filter(|it| it != PLACEHOLDER).collect()
}

fn join_trim(raw: &[String]) -> String {
    let joined = raw.join("\n");
    let t = trim(&joined);
    if t == PLACEHOLDER {
        return String::new();
    }
    t
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Brief,
    What,
    How,
    Signature,
    Condition,
    Pre,
    Post,
    Bugs,
    Suggestions,
    Discard,
}

fn parse_symhow_comment(line: &str, block: &mut NoteBlock, line_no: i32) {
    for caps in kv_re().captures_iter(line) {
        let key = &caps[1];
        let value = caps[2].to_string();
        if key == "symhow-hash" {
            block.body_hash = value;
        } else if key == "symhow-commit" {
            block.commit = value;
        }
    }
    block.meta_line = line_no;
}

/// Collapses runs of whitespace (including newlines) to a single space,
/// trims leading/trailing whitespace, and strips one trailing ';' (with any
/// whitespace before it), if present. Used to normalize `### signature`
/// fence contents so two written-out spacings of the same signature compare
/// equal, and to compare/dedupe overload signatures.
pub fn normalize_signature(signature: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in signature.chars() {
        if is_space(c) {
            if !out.is_empty() && !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    if out.ends_with(';') {
        out.pop();
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

fn flush_section(
    file: &mut NotesFile,
    cur: Option<usize>,
    buf: &mut Vec<(usize, String)>,
    section: Section,
    fence_roles: &[FenceRole],
) {
    let Some(cur) = cur else {
        buf.clear();
        return;
    };
    let b = &mut file.blocks[cur];
    let texts: Vec<String> = buf.iter().map(|(_, t)| t.clone()).collect();
    match section {
        Section::Brief => {
            b.brief = join_trim(&texts);
        }
        Section::What => {
            b.what.text = join_trim(&texts);
        }
        Section::How => {
            b.how.text = join_trim(&texts);
        }
        Section::Signature => {
            // Buffer holds every raw line between the heading and the
            // next heading, including the fence delimiters themselves
            // (fence-interior lines are never mistaken for headings, so
            // they always survive into this buffer intact).
            let mut body_lines: Vec<&str> = Vec::new();
            let mut found_fence = false;
            for (idx, text) in buf.iter() {
                match fence_roles[*idx] {
                    FenceRole::Open => found_fence = true,
                    FenceRole::Body => body_lines.push(text.as_str()),
                    _ => {}
                }
            }
            let sig_line = b.signature.line;
            if !found_fence {
                file.problems.push(Problem {
                    line: sig_line,
                    kind: "missing-signature-fence".to_string(),
                    message: "'### signature' has no ```cpp fence".to_string(),
                });
                b.signature.text = String::new();
            } else {
                let joined = body_lines.join("\n");
                let mut text = normalize_signature(&joined);
                if text == PLACEHOLDER {
                    text = String::new();
                }
                b.signature.text = text;
            }
        }
        Section::Pre => {
            b.pre.items = parse_list_items(&texts);
        }
        Section::Post => {
            b.post.items = parse_list_items(&texts);
        }
        Section::Bugs => {
            b.bugs.items = parse_list_items(&texts);
        }
        Section::Suggestions => {
            b.suggestions.items = parse_list_items(&texts);
        }
        Section::Condition | Section::None | Section::Discard => {}
    }
    buf.clear();
}

/// Parses `content` (the full text of one `<notes-dir>/<source-path>.md`
/// file) into a `NotesFile`. `path` is stored verbatim as
/// `NotesFile::path` (not resolved/read from disk -- callers that have a
/// filesystem path read it themselves and pass both the bytes and the path
/// in).
///
/// The parser never fails: malformed input produces `NotesFile::problems`
/// entries instead. CRLF line endings are tolerated (normalized away).
/// Section order in the file is NOT enforced (order-insensitive parse); only
/// `render_block()`/`render_file()` below impose canonical order.
///
/// When `strict` is true, every section a block doesn't have
/// (what/how/signature/pre/post/bugs/suggestions) adds a "missing-section"
/// Problem.
pub fn parse_notes_file(content: &str, path: &str, strict: bool) -> NotesFile {
    let mut file = NotesFile {
        path: path.to_string(),
        ..Default::default()
    };

    let lines = split_lines(content);
    let fence_roles = scan_fences(&lines, &mut file.problems);

    let mut saw_file_heading = false;
    let mut cur: Option<usize> = None;
    let mut section = Section::None;
    let mut inside_condition = false;
    let mut seen_sections: HashSet<String> = HashSet::new(); // per-block section-name dedupe key
    let mut buf: Vec<(usize, String)> = Vec::new(); // (0-based line idx, raw text) for the active section

    for (i, line) in lines.iter().enumerate() {
        let line_no = (i + 1) as i32;

        if fence_roles[i] != FenceRole::None {
            // Inside (or delimiting) a fence: never a heading. Belongs to
            // whatever section is active (only meaningful for Signature,
            // but harmless to collect elsewhere too).
            if cur.is_none() {
                if !is_blank(line) {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "text-outside-block".to_string(),
                        message: "text before the first '##' block".to_string(),
                    });
                }
            } else {
                buf.push((i, line.clone()));
            }
            continue;
        }

        if let Some(caps) = heading_re().captures(line) {
            let hashes = caps[1].to_string();
            let rest = caps[2].to_string();
            let hash_count = hashes.len();

            if hash_count >= 5 {
                file.problems.push(Problem {
                    line: line_no,
                    kind: "stray-heading-level".to_string(),
                    message: format!("heading level too deep ('{}')", hashes),
                });
                continue;
            }
            if rest.is_empty() || !rest.starts_with(' ') {
                // "##NoSpace" etc -- not a real heading, treat as content.
                if cur.is_none() {
                    if !is_blank(line) {
                        file.problems.push(Problem {
                            line: line_no,
                            kind: "text-outside-block".to_string(),
                            message: "text before the first '##' block".to_string(),
                        });
                    }
                } else {
                    buf.push((i, line.clone()));
                }
                continue;
            }

            let heading_text = trim(&rest);

            if hash_count == 1 {
                flush_section(&mut file, cur, &mut buf, section, &fence_roles);
                section = Section::None;
                inside_condition = false;
                if !saw_file_heading {
                    file.file_heading = heading_text;
                    saw_file_heading = true;
                } else {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "stray-heading-level".to_string(),
                        message: "a second top-level '#' heading".to_string(),
                    });
                }
                continue;
            }

            if hash_count == 2 {
                flush_section(&mut file, cur, &mut buf, section, &fence_roles);
                if heading_text.is_empty() {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "empty-heading".to_string(),
                        message: "'##' heading has no text".to_string(),
                    });
                }
                let block = NoteBlock {
                    heading: heading_text,
                    heading_line: line_no,
                    ..Default::default()
                };
                file.blocks.push(block);
                cur = Some(file.blocks.len() - 1);
                seen_sections.clear();
                section = Section::Brief;
                inside_condition = false;
                continue;
            }

            if hash_count == 3 {
                flush_section(&mut file, cur, &mut buf, section, &fence_roles);
                let Some(c) = cur else {
                    // Stray section heading before any block -- fold into
                    // text-outside-block noise rather than inventing a new
                    // problem kind for an even rarer corner case.
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "text-outside-block".to_string(),
                        message: "'###' heading before the first '##' block".to_string(),
                    });
                    section = Section::Discard;
                    inside_condition = false;
                    continue;
                };
                if heading_text == "what ?" {
                    if seen_sections.contains("what") {
                        file.problems.push(Problem {
                            line: line_no,
                            kind: "duplicate-section".to_string(),
                            message: "duplicate '### what ?'".to_string(),
                        });
                        section = Section::Discard;
                    } else {
                        seen_sections.insert("what".to_string());
                        file.blocks[c].what.present = true;
                        file.blocks[c].what.line = line_no;
                        section = Section::What;
                    }
                    inside_condition = false;
                } else if heading_text == "how ?" {
                    if seen_sections.contains("how") {
                        file.problems.push(Problem {
                            line: line_no,
                            kind: "duplicate-section".to_string(),
                            message: "duplicate '### how ?'".to_string(),
                        });
                        section = Section::Discard;
                    } else {
                        seen_sections.insert("how".to_string());
                        file.blocks[c].how.present = true;
                        file.blocks[c].how.line = line_no;
                        section = Section::How;
                    }
                    inside_condition = false;
                } else if heading_text == "signature" {
                    if seen_sections.contains("signature") {
                        file.problems.push(Problem {
                            line: line_no,
                            kind: "duplicate-section".to_string(),
                            message: "duplicate '### signature'".to_string(),
                        });
                        section = Section::Discard;
                    } else {
                        seen_sections.insert("signature".to_string());
                        file.blocks[c].signature.present = true;
                        file.blocks[c].signature.line = line_no;
                        section = Section::Signature;
                    }
                    inside_condition = false;
                } else if heading_text == "condition" {
                    if seen_sections.contains("condition") {
                        file.problems.push(Problem {
                            line: line_no,
                            kind: "duplicate-section".to_string(),
                            message: "duplicate '### condition'".to_string(),
                        });
                        section = Section::Discard;
                        inside_condition = false;
                    } else {
                        seen_sections.insert("condition".to_string());
                        section = Section::Condition;
                        inside_condition = true;
                    }
                } else {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "unknown-heading".to_string(),
                        message: format!("unknown '### {}'", heading_text),
                    });
                    section = Section::Discard;
                    inside_condition = false;
                }
                continue;
            }

            // hash_count == 4
            flush_section(&mut file, cur, &mut buf, section, &fence_roles);
            let Some(c) = cur.filter(|_| inside_condition) else {
                file.problems.push(Problem {
                    line: line_no,
                    kind: "unknown-heading".to_string(),
                    message: format!("'#### {}' outside '### condition'", heading_text),
                });
                section = Section::Discard;
                continue;
            };
            if heading_text == "pre-condition" {
                if seen_sections.contains("pre-condition") {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "duplicate-section".to_string(),
                        message: "duplicate '#### pre-condition'".to_string(),
                    });
                    section = Section::Discard;
                } else {
                    seen_sections.insert("pre-condition".to_string());
                    file.blocks[c].pre.present = true;
                    file.blocks[c].pre.line = line_no;
                    section = Section::Pre;
                }
            } else if heading_text == "post-condition" {
                if seen_sections.contains("post-condition") {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "duplicate-section".to_string(),
                        message: "duplicate '#### post-condition'".to_string(),
                    });
                    section = Section::Discard;
                } else {
                    seen_sections.insert("post-condition".to_string());
                    file.blocks[c].post.present = true;
                    file.blocks[c].post.line = line_no;
                    section = Section::Post;
                }
            } else if heading_text == "bugs" {
                if seen_sections.contains("bugs") {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "duplicate-section".to_string(),
                        message: "duplicate '#### bugs'".to_string(),
                    });
                    section = Section::Discard;
                } else {
                    seen_sections.insert("bugs".to_string());
                    file.blocks[c].bugs.present = true;
                    file.blocks[c].bugs.line = line_no;
                    section = Section::Bugs;
                }
            } else if heading_text == "suggestions" {
                if seen_sections.contains("suggestions") {
                    file.problems.push(Problem {
                        line: line_no,
                        kind: "duplicate-section".to_string(),
                        message: "duplicate '#### suggestions'".to_string(),
                    });
                    section = Section::Discard;
                } else {
                    seen_sections.insert("suggestions".to_string());
                    file.blocks[c].suggestions.present = true;
                    file.blocks[c].suggestions.line = line_no;
                    section = Section::Suggestions;
                }
            } else {
                file.problems.push(Problem {
                    line: line_no,
                    kind: "unknown-heading".to_string(),
                    message: format!("unknown '#### {}'", heading_text),
                });
                section = Section::Discard;
            }
            continue;
        }

        // Not a heading line.
        if symhow_comment_re().is_match(line) {
            let brief_empty_so_far = buf.iter().all(|(_, t)| is_blank(t));
            let meta_line = cur.map(|c| file.blocks[c].meta_line).unwrap_or(0);
            if cur.is_some() && section == Section::Brief && brief_empty_so_far && meta_line == 0 {
                parse_symhow_comment(line, &mut file.blocks[cur.unwrap()], line_no);
            } else {
                file.problems.push(Problem {
                    line: line_no,
                    kind: "misplaced-meta".to_string(),
                    message: "symhow-hash/symhow-commit comment must immediately follow the '##' heading"
                        .to_string(),
                });
            }
            continue;
        }

        if cur.is_none() {
            if !is_blank(line) {
                file.problems.push(Problem {
                    line: line_no,
                    kind: "text-outside-block".to_string(),
                    message: "text before the first '##' block".to_string(),
                });
            }
            continue;
        }
        buf.push((i, line.clone()));
    }
    flush_section(&mut file, cur, &mut buf, section, &fence_roles);

    // duplicate-block: same heading AND same non-empty normalized
    // signature. Two blocks that share a heading but BOTH have an empty
    // (missing/placeholder) signature are "ambiguous", not "duplicate" --
    // lint has no DB, so it can't tell whether they're accidental repeats
    // or legitimately distinct overloads someone forgot to disambiguate;
    // that call is left to Phase 2's DB-aware `sync`.
    {
        let mut seen_blocks: HashSet<String> = HashSet::new();
        for b in &file.blocks {
            if b.signature.text.is_empty() {
                continue;
            }
            let key = format!("{}\x1f{}", b.heading, b.signature.text);
            if !seen_blocks.insert(key) {
                file.problems.push(Problem {
                    line: b.heading_line,
                    kind: "duplicate-block".to_string(),
                    message: format!("duplicate block '{}' with the same signature", b.heading),
                });
            }
        }
    }

    if strict {
        for b in &file.blocks {
            let push_missing = |problems: &mut Vec<Problem>, msg: &str| {
                problems.push(Problem {
                    line: b.heading_line,
                    kind: "missing-section".to_string(),
                    message: msg.to_string(),
                });
            };
            if !b.what.present {
                push_missing(&mut file.problems, "missing '### what ?'");
            }
            if !b.how.present {
                push_missing(&mut file.problems, "missing '### how ?'");
            }
            if !b.signature.present {
                push_missing(&mut file.problems, "missing '### signature'");
            }
            if !b.pre.present {
                push_missing(&mut file.problems, "missing '#### pre-condition'");
            }
            if !b.post.present {
                push_missing(&mut file.problems, "missing '#### post-condition'");
            }
            if !b.bugs.present {
                push_missing(&mut file.problems, "missing '#### bugs'");
            }
            if !b.suggestions.present {
                push_missing(&mut file.problems, "missing '#### suggestions'");
            }
        }
    }

    file.problems.sort_by_key(|p| p.line); // stable sort, like std::stable_sort

    file
}

fn render_list_section(name: &str, section: &ListSection) -> String {
    let mut out = format!("#### {}\n", name);
    if section.items.is_empty() {
        out.push_str(&format!("1. {}\n", PLACEHOLDER));
    } else {
        for (i, item) in section.items.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, item));
        }
    }
    out
}

/// Renders one block in the canonical format documented in
/// docs/NOTES_DESIGN.md (heading, brief, `### what ?`, `### how ?`,
/// `### signature` fenced with ```cpp, `### condition` with its four `####`
/// subsections). Unfilled sections render as the `_TODO_` placeholder.
/// parse_notes_file(render_block(b)) reproduces b's content (modulo line
/// numbers, which shift to match the rendered text).
pub fn render_block(block: &NoteBlock) -> String {
    let mut s = String::new();
    s.push_str(&format!("## {}\n", block.heading));
    if !block.body_hash.is_empty() || !block.commit.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !block.body_hash.is_empty() {
            parts.push(format!("symhow-hash: {}", block.body_hash));
        }
        if !block.commit.is_empty() {
            parts.push(format!("symhow-commit: {}", block.commit));
        }
        s.push_str("<!-- ");
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                s.push_str("  ");
            }
            s.push_str(part);
        }
        s.push_str(" -->\n");
    }
    s.push_str(&format!("{}\n", if block.brief.is_empty() { PLACEHOLDER.to_string() } else { block.brief.clone() }));
    // Sections that were never present in the source (Phase 1's
    // --empty-ratio-style "missing-section" case, checked only under
    // `lint --strict`) are omitted here too, so parse_notes_file(render_file(f))
    // == f holds even for files with genuinely absent sections, not just
    // fully-populated ones.
    if block.what.present {
        s.push_str("### what ?\n");
        s.push_str(&format!(
            "{}\n",
            if block.what.text.is_empty() { PLACEHOLDER.to_string() } else { block.what.text.clone() }
        ));
    }
    if block.how.present {
        s.push_str("### how ?\n");
        s.push_str(&format!(
            "{}\n",
            if block.how.text.is_empty() { PLACEHOLDER.to_string() } else { block.how.text.clone() }
        ));
    }
    if block.signature.present {
        s.push_str("### signature\n```cpp\n");
        s.push_str(&format!(
            "{}\n",
            if block.signature.text.is_empty() { PLACEHOLDER.to_string() } else { block.signature.text.clone() }
        ));
        s.push_str("```\n");
    }
    if block.pre.present || block.post.present || block.bugs.present || block.suggestions.present {
        s.push_str("### condition\n");
        if block.pre.present {
            s.push_str(&render_list_section("pre-condition", &block.pre));
        }
        if block.post.present {
            s.push_str(&render_list_section("post-condition", &block.post));
        }
        if block.bugs.present {
            s.push_str(&render_list_section("bugs", &block.bugs));
        }
        if block.suggestions.present {
            s.push_str(&render_list_section("suggestions", &block.suggestions));
        }
    }
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

/// Renders a full file: "# <file_heading>\n\n" followed by every block from
/// `render_block()`, each pair of blocks separated by a single blank line.
pub fn render_file(file: &NotesFile) -> String {
    let mut s = format!("# {}\n\n", file.file_heading);
    for (i, block) in file.blocks.iter().enumerate() {
        if i > 0 {
            s.push_str("\n\n");
        }
        s.push_str(&render_block(block));
    }
    s.push('\n');
    s
}

/// Builds a scaffold block for `qualified_name`/`signature` with every
/// free-text/list section left as the `_TODO_` placeholder -- the canonical
/// "unfilled" block. Used by Phase 3's `symhow notes scaffold` and by
/// scripts/symhow-notes-fake.py's placeholder blocks (kept byte-for-byte in
/// sync with this function).
///
/// When `body_hash` and/or `commit` is non-empty, emits the
/// `<!-- symhow-hash: ... symhow-commit: ... -->` staleness-anchor comment
/// right after the heading (see NoteBlock::body_hash's doc comment).
pub fn placeholder_block_markdown(
    qualified_name: &str,
    signature: &str,
    body_hash: &str,
    commit: &str,
) -> String {
    let b = NoteBlock {
        heading: qualified_name.to_string(),
        body_hash: body_hash.to_string(),
        commit: commit.to_string(),
        what: TextSection { present: true, ..Default::default() },
        how: TextSection { present: true, ..Default::default() },
        signature: TextSection {
            present: true,
            text: normalize_signature(signature),
            ..Default::default()
        },
        pre: ListSection { present: true, ..Default::default() },
        post: ListSection { present: true, ..Default::default() },
        bugs: ListSection { present: true, ..Default::default() },
        suggestions: ListSection { present: true, ..Default::default() },
        ..Default::default()
    };
    render_block(&b)
}
