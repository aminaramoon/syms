//! Phase 2 of docs/NOTES_DESIGN.md: computes the "body hash" a note is
//! anchored to -- a hash of the whitespace-normalized text of a symbol's
//! definition span, read straight out of `filecontent`. Used by both
//! `symhow notes sync` (writes it into `symhow_note.current_body_hash`) and the
//! read-path overlay (`notes_store.rs`) to detect staleness without touching
//! the cache. Mirrors `symhow::notes::BodyHash.cpp`.

use crate::common::Result;
use crate::db::enums::location_kind;
use crate::db::meta::get_meta;
use crate::db::Database;
use crate::index::clangutil::{fnv1a64, hex16};

/// std::isspace in the C locale (ASCII only).
fn is_space_byte(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// Collapses runs of whitespace (including newlines) to a single space and
/// trims leading/trailing whitespace -- same normalization `markdown.rs`'s
/// normalize_signature() applies to a signature, applied here to a body span
/// (minus the trailing-';' strip, which a body span doesn't get).
fn normalize_whitespace(s: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut prev_space = false;
    for &c in s {
        if is_space_byte(c) {
            if !out.is_empty() && !prev_space {
                out.push(b' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    out
}

struct Occ {
    file_id: i64,
    file_path: String,
    start_line: i32,
    start_column: i32,
    end_line: i32,
    end_column: i32,
}

fn file_path_for(db: &Database, file_id: i64) -> Result<String> {
    let mut path = String::new();
    db.run("SELECT path FROM file WHERE id = ?", [file_id], |row| {
        path = row.get::<_, Option<String>>(0)?.unwrap_or_default();
        Ok(())
    })?;
    Ok(path)
}

/// Splits `content` into 1-based lines (no trailing newline kept; a '\r'
/// just before a '\n' is dropped) -- BodyHash.cpp's own line splitting,
/// which (unlike markdown.rs's) always keeps a final (possibly empty)
/// segment after the last '\n'.
fn split_lines_bytes(content: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    for (i, &b) in content.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && content[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(&content[start..end]);
            start = i + 1;
        }
    }
    lines.push(&content[start..]);
    lines
}

/// Picks the node's SCOPE occurrence with the largest line span (ties broken
/// by lexicographically smallest file path, then smallest start_line); if no
/// SCOPE occurrence exists, falls back to the TOKEN occurrence's full line.
/// Slices that span's text out of `filecontent.content`, whitespace-
/// normalizes it (runs of whitespace collapsed to one space, trimmed), and
/// hashes it with FNV-1a-64 (`symhow::index::clangutil::fnv1a64`), rendered
/// as 16 lowercase hex digits. Returns "" if the node has no indexed
/// location, or the location's file has no `filecontent` row.
pub fn body_hash_for_node(db: &Database, node_id: i64) -> Result<String> {
    let mut scope_occs: Vec<Occ> = Vec::new();
    let mut token_occs: Vec<Occ> = Vec::new();

    db.run(
        "SELECT l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type \
         FROM occurrence o JOIN source_location l ON l.id = o.source_location_id \
         WHERE o.element_id = ? AND l.type IN (0,1)",
        [node_id],
        |row| {
            let mut occ = Occ {
                file_id: row.get::<_, i64>(0)?,
                start_line: row.get::<_, i32>(1)?,
                start_column: row.get::<_, i32>(2)?,
                end_line: row.get::<_, i32>(3)?,
                end_column: row.get::<_, i32>(4)?,
                file_path: String::new(),
            };
            let kind: i32 = row.get(5)?;
            occ.file_path = file_path_for(db, occ.file_id)?;
            if kind == location_kind::SCOPE {
                scope_occs.push(occ);
            } else {
                token_occs.push(occ);
            }
            Ok(())
        },
    )?;

    let chosen: Option<&Occ> = if !scope_occs.is_empty() {
        // Largest line span; ties -> lexicographically smallest file path,
        // then smallest start_line.
        let mut chosen: Option<&Occ> = None;
        for o in &scope_occs {
            let Some(c) = chosen else {
                chosen = Some(o);
                continue;
            };
            let cur_span = c.end_line - c.start_line;
            let span = o.end_line - o.start_line;
            if span > cur_span {
                chosen = Some(o);
            } else if span == cur_span
                && (o.file_path < c.file_path || (o.file_path == c.file_path && o.start_line < c.start_line))
            {
                chosen = Some(o);
            }
        }
        chosen
    } else if !token_occs.is_empty() {
        let mut chosen: Option<&Occ> = None;
        for o in &token_occs {
            let Some(c) = chosen else {
                chosen = Some(o);
                continue;
            };
            if o.file_path < c.file_path || (o.file_path == c.file_path && o.start_line < c.start_line) {
                chosen = Some(o);
            }
        }
        chosen
    } else {
        None
    };

    let Some(chosen) = chosen else {
        return Ok(String::new());
    };

    // filecontent.content, taken as raw bytes (sqlite3_column_text just
    // reinterprets whatever bytes the column holds).
    let mut content: Option<Vec<u8>> = None;
    db.run("SELECT content FROM filecontent WHERE id = ?", [chosen.file_id], |row| {
        let vref = row.get_ref(0)?;
        let bytes = match vref {
            rusqlite::types::ValueRef::Text(b) => b.to_vec(),
            rusqlite::types::ValueRef::Blob(b) => b.to_vec(),
            _ => Vec::new(),
        };
        content = Some(bytes);
        Ok(())
    })?;
    let Some(content) = content else {
        return Ok(String::new());
    };

    let lines = split_lines_bytes(&content);

    let start_line = chosen.start_line.max(1);
    let end_line = chosen.end_line.max(start_line);
    if start_line > lines.len() as i32 {
        return Ok(String::new());
    }

    let mut span: Vec<u8> = Vec::new();
    let mut ln = start_line;
    while ln <= end_line && ln <= lines.len() as i32 {
        let mut line_text: Vec<u8> = lines[(ln - 1) as usize].to_vec();
        // Clip the trailing edge first (end_column, only meaningful on the
        // last line), then the leading edge (start_column, only meaningful
        // on the first line) -- clamped defensively since column offsets
        // are best-effort and must never panic on an out-of-range value.
        if ln == end_line && chosen.end_column > 0 && chosen.end_column - 1 <= line_text.len() as i32 {
            line_text.truncate((chosen.end_column - 1) as usize);
        }
        if ln == start_line
            && chosen.start_column > 1
            && chosen.start_column - 1 <= line_text.len() as i32
        {
            line_text.drain(..(chosen.start_column - 1) as usize);
        }
        if !span.is_empty() {
            span.push(b'\n');
        }
        span.extend_from_slice(&line_text);
        ln += 1;
    }

    let normalized = normalize_whitespace(&span);
    // fnv1a64() hashes .bytes(); from_utf8_lossy() is a no-op for valid UTF-8
    // content (the only kind the fixture/index write path produces).
    let normalized_str = String::from_utf8_lossy(&normalized).into_owned();
    Ok(hex16(fnv1a64(&normalized_str)))
}

/// Reads `meta.symhow_repo_commit` (empty string if absent).
pub fn current_commit(db: &Database) -> String {
    get_meta(db.conn(), "symhow_repo_commit").unwrap_or_default()
}
