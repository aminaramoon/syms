//! Symbol/file resolution. Mirrors `symhow::query::SymbolResolver`.

use crate::common::Result;
use crate::db::enums::node_kind;
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolveOutcome {
    #[default]
    NotFound,
    Unique,
    Ambiguous,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedSymbol {
    pub id: i64,
    pub kind: i32,
    pub serialized_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveResult {
    pub outcome: ResolveOutcome,
    pub candidates: Vec<ResolvedSymbol>,
}

fn last_segment(spec: &str) -> &str {
    match spec.rfind("::") {
        Some(pos) => &spec[pos + 2..],
        None => spec,
    }
}

// The cheap LIKE pre-filter key: the leading identifier-shaped run of
// lastSegment(spec) (letters/digits/'_'/'~', for destructor names).
fn like_prefilter_key(spec: &str) -> String {
    let seg = last_segment(spec);
    let bytes = seg.as_bytes();
    let mut cut = 0;
    while cut < bytes.len()
        && (bytes[cut].is_ascii_alphanumeric() || bytes[cut] == b'_' || bytes[cut] == b'~')
    {
        cut += 1;
    }
    seg[..cut].to_string()
}

fn finalize(mut candidates: Vec<ResolvedSymbol>) -> ResolveResult {
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    candidates.dedup_by(|a, b| a.id == b.id);

    let outcome = if candidates.is_empty() {
        ResolveOutcome::NotFound
    } else if candidates.len() == 1 {
        ResolveOutcome::Unique
    } else {
        ResolveOutcome::Ambiguous
    };
    ResolveResult { outcome, candidates }
}

pub fn resolve_symbol(
    db: &Database,
    spec: &str,
    kind_filter: Option<&[i32]>,
    signature_filter: Option<&str>,
) -> Result<ResolveResult> {
    if !spec.is_empty() && spec.starts_with('#') {
        let id_text = &spec[1..];
        let id: i64 = match id_text.parse() {
            Ok(v) => v,
            Err(_) => {
                return Ok(ResolveResult {
                    outcome: ResolveOutcome::NotFound,
                    candidates: Vec::new(),
                })
            }
        };

        let mut candidates = Vec::new();
        db.run(
            "SELECT id, type, serialized_name FROM node WHERE id = ?",
            [id],
            |row| {
                candidates.push(ResolvedSymbol {
                    id: row.get::<_, i64>(0)?,
                    kind: row.get::<_, i32>(1)?,
                    serialized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                });
                Ok(())
            },
        )?;
        return Ok(finalize(candidates));
    }

    let prefilter_key = like_prefilter_key(spec);
    let mut candidates = Vec::new();
    db.run(
        "SELECT id, type, serialized_name FROM node WHERE serialized_name LIKE '%' || ? || '%'",
        [prefilter_key.as_str()],
        |row| {
            let id = row.get::<_, i64>(0)?;
            let kind = row.get::<_, i32>(1)?;
            let sname = row.get::<_, Option<String>>(2)?.unwrap_or_default();

            if let Some(kf) = kind_filter {
                if !kf.is_empty() && !kf.contains(&kind) {
                    return Ok(());
                }
            }

            let decoded = decode_serialized_name(&sname);
            let qualified = decoded.qualified_name();
            let display = decoded.display_name();
            let path = decoded.name_path();
            let leaf = decoded
                .elements
                .last()
                .map(|e| e.name.as_str())
                .unwrap_or("");

            let name_matches = spec == qualified || spec == display || spec == path || spec == leaf;
            if !name_matches {
                return Ok(());
            }

            if let Some(sf) = signature_filter {
                if decoded.signature() != *sf {
                    return Ok(());
                }
            }

            candidates.push(ResolvedSymbol {
                id,
                kind,
                serialized_name: sname,
            });
            Ok(())
        },
    )?;

    Ok(finalize(candidates))
}

pub fn resolve_file_path(db: &Database, spec: &str) -> Result<ResolveResult> {
    let mut exact = Vec::new();
    db.run("SELECT id, path FROM file WHERE path = ?", [spec], |row| {
        exact.push(ResolvedSymbol {
            id: row.get::<_, i64>(0)?,
            kind: node_kind::FILE,
            serialized_name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        });
        Ok(())
    })?;
    if !exact.is_empty() {
        return Ok(finalize(exact));
    }

    let mut suffix = Vec::new();
    db.run("SELECT id, path FROM file WHERE path LIKE '%' || ?", [spec], |row| {
        suffix.push(ResolvedSymbol {
            id: row.get::<_, i64>(0)?,
            kind: node_kind::FILE,
            serialized_name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        });
        Ok(())
    })?;
    Ok(finalize(suffix))
}