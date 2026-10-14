//! Phase 3 of docs/NOTES_DESIGN.md: `symhow notes scaffold`. Symbol SELECTION
//! (--all/--file/--class/SYMBOL) is split from note GENERATION
//! (`scaffold_notes`) so the CLI can own the exit-code contract for
//! ambiguous/not-found SYMBOL and --class resolution (mirroring `symhow
//! query`'s own contract) while the actual markdown-writing logic stays a
//! small, independently testable function. Mirrors
//! `symhow::notes::Scaffold.{h,cpp}`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::common::paths::lexical_normalize;
use crate::common::{Error, Result};
use crate::db::enums::node_kind;
use crate::db::meta::get_meta;
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;
use crate::query::QueryEngine;

use super::body_hash::{body_hash_for_node, current_commit};
use super::markdown::{parse_notes_file, placeholder_block_markdown};
use super::sync::{match_block, read_file_lossy, MatchKind};

/// One indexed symbol, resolved to the file it'll be scaffolded into. Shared
/// by `symhow notes symbols` (`list_symbols()`) and `symhow notes scaffold`'s
/// --all/--file/--class/SYMBOL selection (`list_symbols()`/`row_for_node()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolRow {
    pub source_path: String, // relative to symhow_repo_root when resolvable, else the raw definition path
    pub definition_line: i32,
    pub qualified_name: String,
    pub signature: String,
    pub body_hash: String,
    pub commit: String,
    pub node_id: i64,
    pub serialized_name: String,
}

/// Same relative-path derivation runNotesSymbols() (Cli.cpp) used inline
/// before this Phase 3 extraction: relative to `repo_root` when it's known
/// and the location actually lives under it, else the raw definition path
/// verbatim. Mirrors `std::filesystem::relative(filePath, repoRoot)`:
/// weakly-canonicalized operands, then lexically_relative, with any failure
/// falling back to the raw path.
fn relative_to_repo(file_path: &str, repo_root: &str) -> String {
    if repo_root.is_empty() {
        return file_path.to_string();
    }
    let rel = lexically_relative(&weakly_canonical(file_path), &weakly_canonical(repo_root));
    if rel.is_empty() {
        file_path.to_string()
    } else {
        rel
    }
}

/// std::filesystem::weakly_canonical(): canonicalize the longest existing
/// prefix (resolving symlinks), then lexically normalize the rest.
fn weakly_canonical(path: &str) -> String {
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(_) => return lexical_normalize(path),
        }
    };
    if abs.exists() {
        if let Ok(canon) = abs.canonicalize() {
            return canon.to_string_lossy().into_owned();
        }
    }
    lexical_normalize(&abs.to_string_lossy())
}

/// std::filesystem::path::lexically_relative(): shared-component prefix,
/// then "../" for each excess base component, then the remaining components.
fn lexically_relative(p: &str, base: &str) -> String {
    let p_norm = lexical_normalize(p);
    let base_norm = lexical_normalize(base);
    let p_parts: Vec<&str> = p_norm.split('/').filter(|s| !s.is_empty()).collect();
    let base_parts: Vec<&str> = base_norm.split('/').filter(|s| !s.is_empty()).collect();
    let abs_p = p_norm.starts_with('/');
    let abs_base = base_norm.starts_with('/');
    if abs_p != abs_base {
        return String::new();
    }
    let mut common = 0;
    while common < p_parts.len() && common < base_parts.len() && p_parts[common] == base_parts[common] {
        common += 1;
    }
    let mut out_parts: Vec<String> = Vec::new();
    for _ in common..base_parts.len() {
        out_parts.push("..".to_string());
    }
    out_parts.extend(p_parts[common..].iter().map(|s| s.to_string()));
    out_parts.join("/")
}

/// Builds a `SymbolRow` for one already-resolved node id (used by `symhow
/// notes scaffold --class QNAME` -- via `QueryEngine::members()` -- and
/// `symhow notes scaffold SYMBOL`). Returns a default-constructed
/// (`node_id == 0`) `SymbolRow` if the node has no definition location.
pub fn row_for_node(
    db: &Database,
    engine: &QueryEngine,
    node_id: i64,
    repo_root: &str,
    commit: &str,
) -> Result<SymbolRow> {
    let mut row = SymbolRow::default();

    let mut serialized_name = String::new();
    db.run("SELECT serialized_name FROM node WHERE id = ?", [node_id], |r| {
        serialized_name = r.get::<_, Option<String>>(0)?.unwrap_or_default();
        Ok(())
    })?;
    if serialized_name.is_empty() {
        return Ok(row);
    }

    let def = engine.definition(&[node_id])?;
    let Some(loc) = def.locations.first() else {
        return Ok(row);
    };

    row.source_path = relative_to_repo(&loc.file_path, repo_root);
    row.definition_line = loc.start_line;
    let decoded = decode_serialized_name(&serialized_name);
    row.qualified_name = decoded.name_path();
    row.signature = decoded.signature();
    row.body_hash = body_hash_for_node(db, node_id)?;
    row.commit = commit.to_string();
    row.node_id = node_id;
    row.serialized_name = serialized_name;
    Ok(row)
}

/// Every indexed symbol whose node.type matches `kind_mask`
/// (FUNCTION|METHOD when kind_mask == 0), optionally narrowed to source
/// paths matching any of `file_globs` (relative to symhow_repo_root; empty =
/// no filtering), sorted by (source_path, definition_line, qualified_name).
/// This is the exact list `symhow notes symbols` prints and `symhow notes
/// scaffold --all`/`--file` scaffolds.
pub fn list_symbols(db: &Database, kind_mask: i32, file_globs: &[String]) -> Result<Vec<SymbolRow>> {
    let engine = QueryEngine::new(db)?;

    let repo_root = get_meta(db.conn(), "symhow_repo_root").unwrap_or_default();
    let commit = current_commit(db);

    let mask = if kind_mask != 0 {
        kind_mask
    } else {
        node_kind::FUNCTION | node_kind::METHOD
    };

    let mut rows: Vec<SymbolRow> = Vec::new();
    db.run(
        "SELECT n.id FROM node n JOIN symbol s ON s.id = n.id WHERE (n.type & ?) != 0",
        [mask],
        |row| {
            let id: i64 = row.get(0)?;
            let row = row_for_node(db, &engine, id, &repo_root, &commit)?;
            if row.node_id != 0 {
                rows.push(row);
            }
            Ok(())
        },
    )?;

    if !file_globs.is_empty() {
        rows.retain(|r| file_globs.iter().any(|glob| crate::common::glob::matches(glob, &r.source_path)));
    }

    rows.sort_by(|a, b| {
        a.source_path
            .cmp(&b.source_path)
            .then(a.definition_line.cmp(&b.definition_line))
            .then(a.qualified_name.cmp(&b.qualified_name))
    });
    Ok(rows)
}

#[derive(Debug, Clone, Default)]
pub struct ScaffoldOptions {
    pub notes_dir: String,
    pub dry_run: bool,
}

/// Per-target-file outcome of one `scaffold_notes()` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldFileResult {
    pub path: String, // relative to notes_dir, e.g. "src/chain.cpp.md"
    pub file_created: bool,
    pub appended: usize,        // new placeholder blocks written
    pub skipped_existing: usize, // rows that already had a matching block
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldResult {
    pub files: Vec<ScaffoldFileResult>,
    pub files_created: usize,
    pub files_touched: usize, // files_created + files that got >=1 appended block
    pub total_appended: usize,
    pub total_skipped: usize,
}

/// Scaffolds notes for exactly `rows` (already selected by the caller).
/// Idempotent and non-destructive (docs/NOTES_DESIGN.md's "Scaffold" rules):
/// for each row's target file (`<notes_dir>/<source_path>.md`), the existing
/// file's bytes are NEVER rewritten -- if it exists, it's parsed only to find
/// which rows already have a matching block (via `match_block()`), and new
/// placeholder blocks (`placeholder_block_markdown()`) are appended verbatim
/// after its unchanged existing bytes, in definition-line order; a file with
/// nothing new to append is left completely untouched, so running
/// `scaffold_notes()` twice with the same `rows` is a byte-identical no-op
/// the second time. A brand-new file is created with a `# <source_path>`
/// heading. `options.dry_run` computes and returns the same `ScaffoldResult`
/// without writing anything.
pub fn scaffold_notes(db: &Database, rows_in: &[SymbolRow], options: &ScaffoldOptions) -> Result<ScaffoldResult> {
    let mut result = ScaffoldResult::default();

    let mut rows = rows_in.to_vec();
    rows.sort_by(|a, b| {
        a.source_path
            .cmp(&b.source_path)
            .then(a.definition_line.cmp(&b.definition_line))
            .then(a.qualified_name.cmp(&b.qualified_name))
    });

    let mut order: Vec<String> = Vec::new();
    let mut by_source: HashMap<String, Vec<SymbolRow>> = HashMap::new();
    for r in &rows {
        if !by_source.contains_key(&r.source_path) {
            order.push(r.source_path.clone());
        }
        by_source.entry(r.source_path.clone()).or_default().push(r.clone());
    }

    let notes_root = absolute_path(&options.notes_dir)?;

    for source_path in &order {
        let group_rows = &by_source[source_path];
        let rel_path = format!("{}.md", source_path);
        let full = notes_root.join(&rel_path);

        let mut fr = ScaffoldFileResult {
            path: rel_path.clone(),
            ..Default::default()
        };

        let file_exists = full.exists();
        let mut existing_content = String::new();
        let mut existing_node_ids: HashSet<i64> = HashSet::new();
        if file_exists {
            existing_content = read_file_lossy(&full);
            let parsed = parse_notes_file(&existing_content, &rel_path, false);
            for b in &parsed.blocks {
                let mo = match_block(db, b)?;
                if mo.kind == MatchKind::Matched {
                    existing_node_ids.insert(mo.node_id);
                }
            }
        } else {
            fr.file_created = true;
        }

        let mut new_blocks: Vec<String> = Vec::new();
        for row in group_rows {
            if existing_node_ids.contains(&row.node_id) {
                fr.skipped_existing += 1;
                continue;
            }
            new_blocks.push(placeholder_block_markdown(
                &row.qualified_name,
                &row.signature,
                &row.body_hash,
                &row.commit,
            ));
            fr.appended += 1;
        }

        let will_write = !file_exists || fr.appended > 0;
        if !options.dry_run && will_write {
            let out_content = if file_exists {
                let mut s = existing_content.clone();
                while s.ends_with('\n') {
                    s.pop();
                }
                for nb in &new_blocks {
                    s.push_str("\n\n");
                    s.push_str(nb);
                }
                s.push('\n');
                s
            } else {
                let mut s = format!("# {}\n\n", source_path);
                for (i, nb) in new_blocks.iter().enumerate() {
                    if i > 0 {
                        s.push_str("\n\n");
                    }
                    s.push_str(nb);
                }
                s.push('\n');
                s
            };
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Error::new(format!("cannot create directory '{}': {}", parent.display(), e)))?;
            }
            std::fs::write(&full, out_content)
                .map_err(|e| Error::new(format!("cannot write '{}': {}", full.display(), e)))?;
        }

        if fr.file_created {
            result.files_created += 1;
        }
        if fr.file_created || fr.appended > 0 {
            result.files_touched += 1;
        }
        result.total_appended += fr.appended;
        result.total_skipped += fr.skipped_existing;
        result.files.push(fr);
    }

    Ok(result)
}

/// std::filesystem::absolute(): prepend the current working directory, no
/// '.'/'..' normalization (same helper Sync.cpp's syncNotes() uses; kept
/// local here too, mirroring the C++'s per-TU duplication).
fn absolute_path(path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        let cwd =
            std::env::current_dir().map_err(|e| Error::new(format!("cannot determine current directory: {}", e)))?;
        Ok(cwd.join(p))
    }
}
