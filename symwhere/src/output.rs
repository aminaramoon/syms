//! Human and JSON output formatting.
//! CONTRACT: see SPEC.md `output.rs`. Owner: search agent.
use crate::types::{SymbolHit, SymbolKind, SymbolRecord};
use serde_json::{json, Value};

/// Field layout for JSON objects (shared by `emit` and `emit_symbols`).
///
/// Lines are 1-based, columns 0-based (bytes). `signature` is `null` when
/// unknown. `emit_symbols` objects omit `path` (SymbolRecord carries none)
/// and `lang`.
fn hit_json(h: &SymbolHit) -> Value {
    json!({
        "name": h.name,
        "kind": h.kind.name(),
        "container": h.container,
        "path": h.path,
        "lang": h.lang.name(),
        "start_line": h.start_line,
        "start_col": h.start_col,
        "end_line": h.end_line,
        "end_col": h.end_col,
        "signature": h.signature,
    })
}

fn symbol_json(s: &SymbolRecord) -> Value {
    json!({
        "name": s.name,
        "kind": s.kind.name(),
        "container": s.container,
        "start_line": s.start_line,
        "start_col": s.start_col,
        "end_line": s.end_line,
        "end_col": s.end_col,
        "signature": s.signature,
    })
}

/// Serialize records as compact JSON objects, one per line (JSON lines of
/// objects rather than one array line, so streams stay greppable).
fn json_lines(values: &[Value]) -> String {
    let mut out = String::new();
    for v in values {
        out.push_str(&serde_json::to_string(v).expect("json value serialization"));
        out.push('\n');
    }
    out
}

/// Render one hit as `path:line:col  kind  name  [container]  (signature)`.
/// Container is bracketed; signature is a trailing space-separated field.
/// Two-space separators keep the line greppable (`awk -F'  '`).
/// With `spans`, the location gains the definition end: `path:line:col-el:ec`.
pub fn format_hit(h: &SymbolHit, spans: bool) -> String {
    let loc = if spans {
        format!(
            "{}:{}:{}-{}:{}",
            h.path, h.start_line, h.start_col, h.end_line, h.end_col
        )
    } else {
        format!("{}:{}:{}", h.path, h.start_line, h.start_col)
    };
    let mut line = format!("{}  {}  {}", loc, h.kind, h.name);
    if let Some(c) = &h.container {
        line.push_str("  [");
        line.push_str(c);
        line.push(']');
    }
    if let Some(sig) = &h.signature {
        line.push_str("  ");
        line.push_str(sig);
    }
    line
}

/// Render one record as `line:col  kind  name  [container]  (signature)`.
/// (SymbolRecord has no path; single-file output is relative to the file.)
/// With `spans`, the location gains the definition end: `line:col-el:ec`.
pub fn format_symbol(s: &SymbolRecord, spans: bool) -> String {
    let loc = if spans {
        format!(
            "{}:{}-{}:{}",
            s.start_line, s.start_col, s.end_line, s.end_col
        )
    } else {
        format!("{}:{}", s.start_line, s.start_col)
    };
    let mut line = format!("{}  {}  {}", loc, s.kind, s.name);
    if let Some(c) = &s.container {
        line.push_str("  [");
        line.push_str(c);
        line.push(']');
    }
    if let Some(sig) = &s.signature {
        line.push_str("  ");
        line.push_str(sig);
    }
    line
}

/// Print search hits: one compact JSON object per line, or greppable
/// human-readable lines.
pub fn emit(hits: &[SymbolHit], json: bool, spans: bool) {
    if json {
        let values: Vec<Value> = hits.iter().map(hit_json).collect();
        print!("{}", json_lines(&values));
    } else {
        let text = hits
            .iter()
            .map(|h| format_hit(h, spans))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            println!("{text}");
        }
    }
}

/// Print single-file symbols: one compact JSON object per line, or
/// human-readable lines.
pub fn emit_symbols(syms: &[SymbolRecord], json: bool, spans: bool) {
    if json {
        let values: Vec<Value> = syms.iter().map(symbol_json).collect();
        print!("{}", json_lines(&values));
    } else {
        let text = syms
            .iter()
            .map(|s| format_symbol(s, spans))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            println!("{text}");
        }
    }
}

/// Print the supported languages and symbol kinds.
pub fn print_langs() {
    println!("languages:");
    println!("  c");
    println!("  cpp");
    println!("  cuda    (uses the cpp grammar)");
    println!("  rust");
    println!("kinds:");
    for kind in SymbolKind::ALL {
        println!("  {}", kind.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Lang;

    fn hit() -> SymbolHit {
        SymbolHit {
            name: "push".to_string(),
            kind: crate::types::SymbolKind::Method,
            container: Some("ring_buffer".to_string()),
            path: "src/foo.cpp".to_string(),
            lang: Lang::Cpp,
            start_line: 42,
            start_col: 10,
            end_line: 45,
            end_col: 1,
            signature: Some("void ring_buffer::push(T v)".to_string()),
        }
    }

    #[test]
    fn human_hit_layout_is_greppable() {
        assert_eq!(
            format_hit(&hit(), false),
            "src/foo.cpp:42:10  method  push  [ring_buffer]  void ring_buffer::push(T v)"
        );
    }

    #[test]
    fn human_hit_spans_mode_includes_end() {
        assert_eq!(
            format_hit(&hit(), true),
            "src/foo.cpp:42:10-45:1  method  push  [ring_buffer]  void ring_buffer::push(T v)"
        );
    }

    #[test]
    fn human_hit_omits_missing_container_and_signature() {
        let mut h = hit();
        h.container = None;
        h.signature = None;
        assert_eq!(format_hit(&h, false), "src/foo.cpp:42:10  method  push");
    }

    #[test]
    fn human_symbol_layout() {
        let s = hit();
        let rec = SymbolRecord {
            name: s.name.clone(),
            kind: s.kind,
            container: s.container.clone(),
            start_line: s.start_line,
            start_col: s.start_col,
            end_line: s.end_line,
            end_col: s.end_col,
            signature: s.signature.clone(),
        };
        assert_eq!(
            format_symbol(&rec, false),
            "42:10  method  push  [ring_buffer]  void ring_buffer::push(T v)"
        );
    }

    #[test]
    fn human_symbol_spans_mode_includes_end() {
        let s = hit();
        let rec = SymbolRecord {
            name: s.name.clone(),
            kind: s.kind,
            container: s.container.clone(),
            start_line: s.start_line,
            start_col: s.start_col,
            end_line: s.end_line,
            end_col: s.end_col,
            signature: s.signature.clone(),
        };
        assert_eq!(
            format_symbol(&rec, true),
            "42:10-45:1  method  push  [ring_buffer]  void ring_buffer::push(T v)"
        );
    }

    #[test]
    fn hit_json_has_all_fields_and_round_trips() {
        let text = json_lines(&[hit_json(&hit())]);
        let v: Value = serde_json::from_str(text.trim_end()).unwrap();
        let obj = v.as_object().map(|o| o.len());
        assert_eq!(obj, Some(10));
        assert_eq!(v["name"], "push");
        assert_eq!(v["kind"], "method");
        assert_eq!(v["container"], "ring_buffer");
        assert_eq!(v["path"], "src/foo.cpp");
        assert_eq!(v["lang"], "cpp");
        assert_eq!(v["start_line"], 42);
        assert_eq!(v["start_col"], 10);
        assert_eq!(v["end_line"], 45);
        assert_eq!(v["end_col"], 1);
        assert_eq!(v["signature"], "void ring_buffer::push(T v)");
    }

    #[test]
    fn symbol_json_omits_path_and_lang() {
        let rec = SymbolRecord {
            name: "push".to_string(),
            kind: crate::types::SymbolKind::Method,
            container: None,
            start_line: 42,
            start_col: 10,
            end_line: 45,
            end_col: 1,
            signature: None,
        };
        let v: Value = serde_json::from_str(json_lines(&[symbol_json(&rec)]).trim_end()).unwrap();
        assert!(v.get("path").is_none());
        assert!(v.get("lang").is_none());
        assert_eq!(v["container"], Value::Null);
        assert_eq!(v["signature"], Value::Null);
        assert_eq!(v["start_line"], 42);
        assert_eq!(v["start_col"], 10);
    }

    #[test]
    fn json_lines_produces_one_compact_object_per_line() {
        let text = json_lines(&[hit_json(&hit()), hit_json(&hit())]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines
            .iter()
            .all(|l| serde_json::from_str::<Value>(l).is_ok()));
    }
}
