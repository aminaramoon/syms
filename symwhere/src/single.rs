//! Single-file symbol listing (stateless, no database).
//! CONTRACT: see SPEC.md `single.rs`. Owner: search agent.
use crate::index::parse_file;
use crate::lang::LangRegistry;
use crate::types::{Lang, SymbolKind, SymbolRecord};
use anyhow::Context;
use std::collections::BTreeSet;
use std::path::PathBuf;

pub struct SymbolsOptions {
    pub file: PathBuf,
    pub kinds: BTreeSet<SymbolKind>,
    pub within: Option<String>,
    pub lang: Option<Lang>,
}

/// Parse one file and list its symbols.
///
/// With `within = "N"` (or `"A-B"`, 1-based inclusive), only symbols whose
/// span encloses the line/range are returned, ordered smallest span first
/// (innermost first). If nothing encloses the range, all (kind-filtered)
/// symbols are returned in source order and a note is printed to stderr.
pub fn run_symbols(opts: SymbolsOptions) -> anyhow::Result<Vec<SymbolRecord>> {
    let lang = opts
        .lang
        .or_else(|| Lang::detect(&opts.file))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot detect language for `{}`; pass --lang (c, cpp, cuda, rust)",
                opts.file.display()
            )
        })?;
    let src = std::fs::read(&opts.file)
        .with_context(|| format!("failed to read `{}`", opts.file.display()))?;
    let registry = LangRegistry::new();
    let mut syms = parse_file(&opts.file, lang, &src, &registry)
        .with_context(|| format!("failed to parse `{}` as {}", opts.file.display(), lang))?;
    syms.retain(|s| opts.kinds.contains(&s.kind));

    match opts.within.as_deref().map(parse_within).transpose()? {
        Some((lo, hi)) => {
            let has_enclosing = syms.iter().any(|s| s.start_line <= lo && s.end_line >= hi);
            if has_enclosing {
                return Ok(filter_within(syms, lo, hi));
            }
            eprintln!(
                "note: no symbols enclose line(s) {}; showing all symbols",
                if lo == hi {
                    lo.to_string()
                } else {
                    format!("{lo}-{hi}")
                }
            );
            syms.sort_by_key(|s| (s.start_line, s.start_col));
            Ok(syms)
        }
        None => {
            syms.sort_by_key(|s| (s.start_line, s.start_col));
            Ok(syms)
        }
    }
}

/// Parse a `within` value: `"N"` (single line) or `"A-B"` (1-based inclusive
/// range, `A <= B`).
fn parse_within(s: &str) -> anyhow::Result<(u32, u32)> {
    let parse = |v: &str| -> anyhow::Result<u32> {
        v.trim()
            .parse::<u32>()
            .with_context(|| format!("invalid line number `{v}` in --within `{s}`"))
    };
    match s.trim().split_once('-') {
        Some((a, b)) => {
            let lo = parse(a)?;
            let hi = parse(b)?;
            if lo > hi {
                anyhow::bail!("invalid range in --within `{s}`: start is after end");
            }
            Ok((lo, hi))
        }
        None => {
            let line = parse(s)?;
            Ok((line, line))
        }
    }
}

/// Keep symbols whose `[start_line, end_line]` span encloses `[lo, hi]`,
/// ordered by span size ascending (innermost first).
fn filter_within(syms: Vec<SymbolRecord>, lo: u32, hi: u32) -> Vec<SymbolRecord> {
    let mut kept: Vec<SymbolRecord> = syms
        .into_iter()
        .filter(|s| s.start_line <= lo && s.end_line >= hi)
        .collect();
    kept.sort_by_key(|s| (s.end_line - s.start_line, s.start_line, s.start_col));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, start: u32, end: u32) -> SymbolRecord {
        SymbolRecord {
            name: name.to_string(),
            kind: SymbolKind::Function,
            container: None,
            start_line: start,
            start_col: 0,
            end_line: end,
            end_col: 0,
            signature: None,
        }
    }

    fn syms() -> Vec<SymbolRecord> {
        // Sorted by line, as the parser would emit them.
        vec![
            rec("outer", 10, 100),
            rec("mid", 20, 90),
            rec("inner", 30, 40),
            rec("tiny", 31, 32),
            rec("sibling", 50, 60),
            rec("after", 120, 130),
        ]
    }

    #[test]
    fn parses_single_line() {
        assert_eq!(parse_within("42").unwrap(), (42, 42));
    }

    #[test]
    fn parses_range() {
        assert_eq!(parse_within("10-20").unwrap(), (10, 20));
    }

    #[test]
    fn rejects_reversed_range() {
        assert!(parse_within("20-10").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_within("abc").is_err());
        assert!(parse_within("10-x").is_err());
        assert!(parse_within("-5").is_err());
    }

    #[test]
    fn keeps_only_enclosing_symbols_innermost_first() {
        let out = filter_within(syms(), 32, 33);
        // Spans covering [32, 33]; smallest span first ("tiny" ends at 32 and
        // so does not cover 33).
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["inner", "mid", "outer"]);
    }

    #[test]
    fn single_line_within_encloses_correctly() {
        let out = filter_within(syms(), 55, 55);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        // "sibling" is the innermost symbol containing line 55; "mid" and
        // "outer" also enclose it and follow, largest last.
        assert_eq!(names, vec!["sibling", "mid", "outer"]);
    }

    #[test]
    fn boundary_lines_are_inclusive() {
        let out = filter_within(syms(), 10, 100);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "outer");
    }

    #[test]
    fn range_outside_every_symbol_is_empty() {
        let out = filter_within(syms(), 200, 210);
        assert!(out.is_empty());
    }

    #[test]
    fn partial_overlap_does_not_count_as_enclosing() {
        // [95, 105] overlaps `outer` (10-100) but does not enclose it.
        let out = filter_within(syms(), 95, 105);
        assert!(out.is_empty());
    }
}
