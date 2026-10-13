//! Fuzzy symbol search over the index.
//! CONTRACT: see SPEC.md `fuzzy.rs`. Owner: search agent.
use crate::store::{Store, SymbolRow};
use crate::types::{Lang, SymbolHit, SymbolKind};
use crate::walk::glob_match;
use anyhow::Context;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;

pub struct FindOptions {
    pub queries: Vec<String>,
    pub kinds: BTreeSet<SymbolKind>,
    pub langs: BTreeSet<Lang>,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub exact: bool,
    pub limit: usize,
    pub db: Option<PathBuf>,
}

/// Run a `find` query against the index database.
pub fn run_find(opts: FindOptions) -> anyhow::Result<Vec<SymbolHit>> {
    let db = locate_db(opts.db.as_deref())?;
    let store = Store::open(&db, false)
        .with_context(|| format!("failed to open index `{}`", db.display()))?;
    let hits = store
        .all_symbols()
        .context("failed to read symbols from the index")?;
    Ok(rank(
        hard_filter(rows_to_hits(hits), &opts),
        &opts.queries,
        opts.exact,
        opts.limit,
    ))
}

/// Convert raw DB rows into [`SymbolHit`]s. Rows with an unrecognized kind or
/// language are skipped rather than failing the whole search (the index only
/// ever writes canonical strings, so this should never trigger).
fn rows_to_hits(rows: Vec<SymbolRow>) -> Vec<SymbolHit> {
    rows.into_iter()
        .filter_map(
            |(path, lang, name, kind, container, sl, sc, el, ec, signature)| {
                let kind = FromStr::from_str(&kind).ok()?;
                Some(SymbolHit {
                    name,
                    kind,
                    container,
                    path,
                    lang,
                    start_line: clamp_u32(sl),
                    start_col: clamp_u32(sc),
                    end_line: clamp_u32(el),
                    end_col: clamp_u32(ec),
                    signature,
                })
            },
        )
        .collect()
}

fn clamp_u32(v: i64) -> u32 {
    v.clamp(0, u32::MAX as i64) as u32
}

/// Apply the non-fuzzy hard filters: kind set, language set, and path
/// include/exclude globs. Includes (when non-empty) are OR-ed; any matching
/// exclude rejects the path.
fn hard_filter(hits: Vec<SymbolHit>, opts: &FindOptions) -> Vec<SymbolHit> {
    hits.into_iter()
        .filter(|h| opts.kinds.contains(&h.kind))
        .filter(|h| opts.langs.contains(&h.lang))
        .filter(|h| path_allowed(&h.path, &opts.includes, &opts.excludes))
        .collect()
}

fn path_allowed(path: &str, includes: &[String], excludes: &[String]) -> bool {
    if !includes.is_empty() && !includes.iter().any(|p| glob_match(p, path)) {
        return false;
    }
    !excludes.iter().any(|p| glob_match(p, path))
}

/// Rank, order and truncate `hits` against `queries`.
///
/// A candidate is kept when it matches at least one query (OR semantics); the
/// best score across queries wins. Candidates whose bare `name` matches rank
/// above candidates that only match `path::name`. `exact` switches to a
/// case-insensitive substring filter instead of fuzzy scoring. With no
/// queries, filter survivors are returned in their original order, capped at
/// `limit`.
///
/// Pure (no I/O), so tests can drive it directly.
pub fn rank(hits: Vec<SymbolHit>, queries: &[String], exact: bool, limit: usize) -> Vec<SymbolHit> {
    if limit == 0 {
        return Vec::new();
    }
    if queries.is_empty() {
        let mut hits = hits;
        hits.truncate(limit);
        return hits;
    }

    let mut ranked: Vec<(RankKey, SymbolHit)> = if exact {
        rank_exact(hits, queries)
    } else {
        rank_fuzzy(hits, queries)
    };
    // Score descending, then name ascending for stable, deterministic output.
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.name.cmp(&b.1.name))
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.1.start_line.cmp(&b.1.start_line))
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, h)| h).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct RankKey {
    /// 1 = matched on the bare name, 0 = only on `path::name`.
    name_rank: u8,
    /// Primary match score (higher is better).
    score: u32,
    /// Secondary score over the full `path::name` haystack.
    full_score: u32,
}

fn rank_fuzzy(hits: Vec<SymbolHit>, queries: &[String]) -> Vec<(RankKey, SymbolHit)> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let patterns: Vec<Pattern> = queries
        .iter()
        .map(|q| Pattern::parse(q, CaseMatching::Smart, Normalization::Smart))
        .collect();

    let mut buf = Vec::new();
    let mut ranked = Vec::with_capacity(hits.len());
    for hit in hits {
        let full = full_key(&hit);
        let mut best: Option<RankKey> = None;
        for pattern in &patterns {
            let name_score = score(pattern, &hit.name, &mut matcher, &mut buf);
            let full_score = score(pattern, &full, &mut matcher, &mut buf);
            // Prefer a bare-name match; fall back to a path::name match.
            let key = match (name_score, full_score) {
                (Some(a), Some(b)) => RankKey {
                    name_rank: 1,
                    score: a,
                    full_score: b,
                },
                (Some(a), None) => RankKey {
                    name_rank: 1,
                    score: a,
                    full_score: a,
                },
                (None, Some(b)) => RankKey {
                    name_rank: 0,
                    score: b,
                    full_score: b,
                },
                (None, None) => continue,
            };
            best = Some(match best {
                Some(prev) if prev >= key => prev,
                _ => key,
            });
        }
        if let Some(key) = best {
            ranked.push((key, hit));
        }
    }
    ranked
}

/// Score one haystack against one pattern. `buf` is scratch storage reused
/// across calls to avoid an allocation per (candidate × query) pair.
fn score(
    pattern: &Pattern,
    haystack: &str,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
) -> Option<u32> {
    buf.clear();
    pattern.score(Utf32Str::new(haystack, buf), matcher)
}

/// Case-insensitive substring filtering; a hit matches when at least one
/// query is a substring of `name` (ranked above) or of `path::name`.
fn rank_exact(hits: Vec<SymbolHit>, queries: &[String]) -> Vec<(RankKey, SymbolHit)> {
    let needles: Vec<String> = queries.iter().map(|q| q.to_lowercase()).collect();
    let mut ranked = Vec::new();
    for hit in hits {
        let name_ci = hit.name.to_lowercase();
        let full_ci = full_key(&hit).to_lowercase();
        let mut best: Option<RankKey> = None;
        for needle in &needles {
            let key = if name_ci.contains(needle.as_str()) {
                RankKey {
                    name_rank: 1,
                    score: u32::MAX,
                    full_score: 0,
                }
            } else if full_ci.contains(needle.as_str()) {
                RankKey {
                    name_rank: 0,
                    score: 0,
                    full_score: 0,
                }
            } else {
                continue;
            };
            best = Some(match best {
                Some(prev) if prev >= key => prev,
                _ => key,
            });
        }
        if let Some(key) = best {
            ranked.push((key, hit));
        }
    }
    ranked
}

/// `path::name` — the secondary haystack for ranking.
fn full_key(h: &SymbolHit) -> String {
    format!("{}::{}", h.path, h.name)
}

/// Resolve the index database for read-only commands: an explicit `--db` wins;
/// otherwise walk up from the cwd looking for a `.symwhere/index.db`, git-style.
pub fn locate_db(explicit: Option<&std::path::Path>) -> anyhow::Result<PathBuf> {
    if let Some(db) = explicit {
        let db = if db.is_dir() {
            db.join("index.db")
        } else {
            db.to_path_buf()
        };
        if !db.exists() {
            anyhow::bail!(
                "no index database at {}; run `symwhere index` first",
                db.display()
            );
        }
        return Ok(db);
    }
    let cwd = std::fs::canonicalize(".").context("failed to resolve the current directory")?;
    let mut dir: Option<&std::path::Path> = Some(cwd.as_path());
    while let Some(d) = dir {
        let candidate = d.join(".symwhere").join("index.db");
        if candidate.is_file() {
            return Ok(candidate);
        }
        dir = d.parent();
    }
    anyhow::bail!(
        "no symbol index found in {} or any parent; run `symwhere index` first (or pass --db)",
        cwd.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, kind: SymbolKind, path: &str, container: Option<&str>) -> SymbolHit {
        SymbolHit {
            name: name.to_string(),
            kind,
            container: container.map(str::to_string),
            path: path.to_string(),
            lang: Lang::Rust,
            start_line: 1,
            start_col: 0,
            end_line: 2,
            end_col: 0,
            signature: None,
        }
    }

    fn hits() -> Vec<SymbolHit> {
        vec![
            hit("ring_buffer", SymbolKind::Struct, "src/buf.rs", None),
            hit(
                "push",
                SymbolKind::Method,
                "src/buf.rs",
                Some("ring_buffer"),
            ),
            hit("Buffer", SymbolKind::Class, "src/other.rs", None),
            hit("unrelated", SymbolKind::Function, "src/other.rs", None),
            hit("buffer_len", SymbolKind::Function, "include/buf.h", None),
            hit("buffer2", SymbolKind::Struct, "gen/buf.rs", None),
        ]
    }

    fn names(out: &[SymbolHit]) -> Vec<&str> {
        out.iter().map(|h| h.name.as_str()).collect()
    }

    #[test]
    fn fuzzy_matches_prefix_and_infix() {
        // "buf" is a prefix of "buffer2"/"Buffer"/"buffer_len", an infix of
        // "ring_buffer", and matches "push" only via its path `src/buf.rs`
        // (path-only matches rank below bare-name matches).
        let out = rank(hits(), &["buf".to_string()], false, 10);
        assert_eq!(
            names(&out),
            vec!["Buffer", "buffer2", "buffer_len", "ring_buffer", "push"]
        );
    }

    #[test]
    fn smart_case_is_respected_for_uppercase_queries() {
        // Uppercase in the query => case-sensitive ("Buffer" only).
        let out = rank(hits(), &["Buf".to_string()], false, 10);
        assert_eq!(names(&out), vec!["Buffer"]);
    }

    #[test]
    fn lowercase_query_ignores_case() {
        let out = rank(hits(), &["ufer".to_string()], false, 10);
        assert!(names(&out).contains(&"Buffer"));
    }

    #[test]
    fn unmatched_candidates_are_dropped() {
        let out = rank(hits(), &["zzzz".to_string()], false, 10);
        assert!(out.is_empty());
    }

    #[test]
    fn multi_query_is_or_semantics_with_best_score() {
        let queries = ["push".to_string(), "buf".to_string()];
        let out = rank(hits(), &queries, false, 10);
        // OR semantics: every kept hit matched at least one query, and hits
        // from both query groups are present.
        assert!(names(&out).contains(&"push"));
        assert!(names(&out).contains(&"ring_buffer"));
        for h in &out {
            assert!(
                queries
                    .iter()
                    .any(|q| h.name.to_lowercase().contains(&q.to_lowercase())),
                "hit {:?} matched no query",
                h.name
            );
        }
    }

    #[test]
    fn no_queries_returns_all_capped() {
        let out = rank(hits(), &[], false, 3);
        assert_eq!(out.len(), 3);
        let out = rank(hits(), &[], false, 100);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn limit_truncates_results() {
        let out = rank(hits(), &["buf".to_string()], false, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(names(&out), vec!["Buffer", "buffer2"]);
    }

    #[test]
    fn zero_limit_returns_nothing() {
        assert!(rank(hits(), &["buf".to_string()], false, 0).is_empty());
    }

    #[test]
    fn exact_mode_is_case_insensitive_substring() {
        let out = rank(hits(), &["FFER".to_string()], true, 10);
        assert_eq!(
            names(&out),
            vec!["Buffer", "buffer2", "buffer_len", "ring_buffer"]
        );
    }

    #[test]
    fn exact_mode_requires_contiguous_substring() {
        let out = rank(hits(), &["bff".to_string()], true, 10);
        assert!(out.is_empty());
    }

    #[test]
    fn exact_mode_falls_back_to_path_name() {
        let mut h = hit("plain", SymbolKind::Function, "src/buffer_dir/mod.rs", None);
        h.lang = Lang::Cpp;
        let out = rank(vec![h.clone()], &["buffer_dir".to_string()], true, 10);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn hard_filter_applies_kind_lang_and_globs() {
        let opts = FindOptions {
            queries: vec![],
            kinds: [SymbolKind::Function, SymbolKind::Method]
                .into_iter()
                .collect(),
            langs: [Lang::Rust].into_iter().collect(),
            excludes: vec!["gen/**".to_string()],
            includes: vec!["src/**".to_string()],
            exact: false,
            limit: 50,
            db: None,
        };
        let out = hard_filter(hits(), &opts);
        // "buffer_len" (include/buf.h) filtered by include glob; unrelated
        // survives on glob+lang but not kind; buffer2 dropped by exclude.
        assert_eq!(names(&out), vec!["push", "unrelated"]);
    }

    #[test]
    fn empty_include_set_means_no_include_restriction() {
        let opts = FindOptions {
            queries: vec![],
            kinds: SymbolKind::ALL.into_iter().collect(),
            langs: Lang::ALL.into_iter().collect(),
            excludes: vec!["gen/**".to_string()],
            includes: vec![],
            exact: false,
            limit: 50,
            db: None,
        };
        let out = hard_filter(hits(), &opts);
        assert_eq!(out.len(), 5);
    }
}
