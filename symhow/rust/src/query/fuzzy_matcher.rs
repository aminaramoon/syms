//! Clean-room minimal fuzzy matcher. Mirrors `symhow::query::FuzzyMatcher`:
//! greedy in-order subsequence match + positional bonuses.

/// One fuzzy match (mirrors `symhow::query::FuzzyHit`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuzzyHit {
    pub node_id: i64,
    pub score: i32,
    /// Into the ORIGINAL (non-lowercased) candidate text.
    pub matched_indices: Vec<i32>,
}

fn to_lower(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c })
        .collect()
}

// Bit `c - 'a'` for lowercase letters; every other character is spread
// across the remaining 38 bits via a cheap hash. False positives only ever
// widen the fast pre-filter, never narrow it.
fn char_bit(c: u8) -> u64 {
    if (b'a'..=b'z').contains(&c) {
        1u64 << (c - b'a')
    } else {
        1u64 << (26 + (c % 38))
    }
}

fn build_bitset(lower: &str) -> u64 {
    let mut bits = 0u64;
    for c in lower.bytes() {
        bits |= char_bit(c);
    }
    bits
}

fn is_boundary_char(c: u8) -> bool {
    c == b'.' || c == b'_' || c == b':' || c == b'/' || c == b'\\'
}

struct GreedyMatch {
    score: i32,
    indices: Vec<i32>,
}

fn greedy_score(
    pattern_lower: &str,
    text_lower: &str,
    text_original: &str,
) -> Option<GreedyMatch> {
    if pattern_lower.is_empty() {
        return Some(GreedyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }

    let mut m = GreedyMatch {
        score: 0,
        indices: Vec::with_capacity(pattern_lower.len()),
    };

    let original_bytes = text_original.as_bytes();

    let mut cursor = 0usize;
    let mut prev_idx: i32 = -1;
    let mut first_idx: i32 = -1;

    for pc in pattern_lower.chars() {
        let found = text_lower[cursor..].find(pc)?;
        let idx = (cursor + found) as i32;

        if first_idx < 0 {
            first_idx = idx;
        }
        if idx == 0 {
            m.score += 4;
        }
        if prev_idx >= 0 {
            if idx == prev_idx + 1 {
                m.score += 4;
            } else {
                let gap = idx - prev_idx;
                m.score -= gap - 1;
            }
        }
        if idx > 0 && is_boundary_char(original_bytes[(idx - 1) as usize]) {
            m.score += 4;
        }
        if idx > 0 {
            let cur_orig = original_bytes[idx as usize];
            let prev_orig = original_bytes[(idx - 1) as usize];
            if cur_orig.is_ascii_uppercase() && prev_orig.is_ascii_lowercase() {
                m.score += 3;
            }
        }

        m.indices.push(idx);
        prev_idx = idx;
        cursor = idx as usize + 1;
    }

    if first_idx >= 0 {
        m.score += std::cmp::max(-first_idx, -20);
    }
    Some(m)
}

struct Scored {
    hit: FuzzyHit,
    text_len: usize,
}

/// Scores every candidate whose lowercased text contains `pattern`'s
/// (lowercased) characters as an in-order subsequence. Results are sorted
/// descending by score, ties broken by (shorter text, then lower node_id),
/// then truncated to `limit` (no truncation if `limit <= 0`).
pub fn fuzzy_match(pattern: &str, candidates: &[(i64, String)], limit: i32) -> Vec<FuzzyHit> {
    let pattern_lower = to_lower(pattern);
    let pattern_bits = build_bitset(&pattern_lower);

    let mut scored: Vec<Scored> = Vec::with_capacity(candidates.len());

    for (id, text) in candidates {
        let text_lower = to_lower(text);
        let text_bits = build_bitset(&text_lower);
        if (pattern_bits & text_bits) != pattern_bits {
            continue;
        }

        let m = match greedy_score(&pattern_lower, &text_lower, text) {
            Some(m) => m,
            None => continue,
        };

        scored.push(Scored {
            hit: FuzzyHit {
                node_id: *id,
                score: m.score,
                matched_indices: m.indices,
            },
            text_len: text.len(),
        });
    }

    scored.sort_by(|a, b| {
        b.hit.score
            .cmp(&a.hit.score)
            .then(a.text_len.cmp(&b.text_len))
            .then(a.hit.node_id.cmp(&b.hit.node_id))
    });

    if limit > 0 && scored.len() > limit as usize {
        scored.truncate(limit as usize);
    }

    scored.into_iter().map(|s| s.hit).collect()
}