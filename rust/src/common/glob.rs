//! Small glob matcher. Mirrors `symhow::common::glob`.
//!
//! Supported syntax: `*` (any run excluding `/`), `**` (any run including
//! `/`), `?` (exactly one char), `[...]` (char class, leading `!`/`^`
//! negates, `a-z` ranges). Everything else matches literally.

#[derive(Clone, PartialEq)]
enum Kind {
    Literal,
    AnyChar,
    CharClass,
    Star,
    GlobStar,
}

#[derive(Clone)]
struct Unit {
    kind: Kind,
    literal: char,
    negate: bool,
    ranges: Vec<(char, char)>,
}

impl Unit {
    fn literal(c: char) -> Unit {
        Unit { kind: Kind::Literal, literal: c, negate: false, ranges: Vec::new() }
    }
}

fn char_class_matches(unit: &Unit, c: char) -> bool {
    let in_set = unit.ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
    if unit.negate {
        !in_set
    } else {
        in_set
    }
}

fn unit_matches_char(unit: &Unit, c: char) -> bool {
    match unit.kind {
        Kind::Literal => unit.literal == c,
        Kind::AnyChar => true,
        Kind::CharClass => char_class_matches(unit, c),
        _ => false, // Star/GlobStar handled separately
    }
}

fn tokenize(pattern: &str) -> Vec<Unit> {
    let mut units = Vec::new();
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let c = chars[i];

        if c == '*' {
            if i + 1 < n && chars[i + 1] == '*' {
                units.push(Unit { kind: Kind::GlobStar, literal: '\0', negate: false, ranges: Vec::new() });
                i += 2;
                // Collapse any additional consecutive '*' into the same globstar.
                while i < n && chars[i] == '*' {
                    i += 1;
                }
            } else {
                units.push(Unit { kind: Kind::Star, literal: '\0', negate: false, ranges: Vec::new() });
                i += 1;
            }
            continue;
        }

        if c == '?' {
            units.push(Unit { kind: Kind::AnyChar, literal: '\0', negate: false, ranges: Vec::new() });
            i += 1;
            continue;
        }

        if c == '[' {
            let mut j = i + 1;
            let mut negate = false;
            if j < n && (chars[j] == '!' || chars[j] == '^') {
                negate = true;
                j += 1;
            }
            let content_start = j;
            // Find closing ']' from content_start.
            let close_idx = (content_start..n).find(|&k| chars[k] == ']');
            match close_idx {
                None => {
                    // No closing bracket: treat '[' as a literal character.
                    units.push(Unit::literal('['));
                    i += 1;
                    continue;
                }
                Some(close_idx) => {
                    let mut u = Unit { kind: Kind::CharClass, literal: '\0', negate, ranges: Vec::new() };
                    let mut k = content_start;
                    while k < close_idx {
                        if k + 2 < close_idx && chars[k + 1] == '-' {
                            u.ranges.push((chars[k], chars[k + 2]));
                            k += 3;
                        } else {
                            u.ranges.push((chars[k], chars[k]));
                            k += 1;
                        }
                    }
                    units.push(u);
                    i = close_idx + 1;
                    continue;
                }
            }
        }

        units.push(Unit::literal(c));
        i += 1;
    }

    units
}

/// Returns true if `path` matches `pattern` in full.
pub fn matches(pattern: &str, path: &str) -> bool {
    let units = tokenize(pattern);
    let path_chars: Vec<char> = path.chars().collect();
    let mut memo = vec![vec![-1i8; path_chars.len() + 1]; units.len() + 1];
    match_rec(&units, 0, &path_chars, 0, &mut memo)
}

fn match_rec(units: &[Unit], ui: usize, str: &[char], si: usize, memo: &mut Vec<Vec<i8>>) -> bool {
    if memo[ui][si] != -1 {
        return memo[ui][si] != 0;
    }

    let result = if ui == units.len() {
        si == str.len()
    } else if units[ui].kind == Kind::Star {
        // Zero-length match first, then consume one more non-'/' char.
        match_rec(units, ui + 1, str, si, memo)
            || (si < str.len() && str[si] != '/' && match_rec(units, ui, str, si + 1, memo))
    } else if units[ui].kind == Kind::GlobStar {
        // Zero-length match, continuing right after the globstar.
        let mut r = match_rec(units, ui + 1, str, si, memo);

        // Segment-collapse: "a/**/b" should also match "a/b".
        if !r
            && ui + 1 < units.len()
            && units[ui + 1].kind == Kind::Literal
            && units[ui + 1].literal == '/'
        {
            r = match_rec(units, ui + 2, str, si, memo);
        }

        if !r && si < str.len() {
            r = match_rec(units, ui, str, si + 1, memo);
        }
        r
    } else if si < str.len() && unit_matches_char(&units[ui], str[si]) {
        match_rec(units, ui + 1, str, si + 1, memo)
    } else {
        false
    };

    memo[ui][si] = if result { 1 } else { 0 };
    result
}