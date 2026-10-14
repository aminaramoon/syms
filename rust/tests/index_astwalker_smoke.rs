//! End-to-end smoke test of the libclang core: parse a tiny TU, walk it with
//! AstWalker, and check the emitted facts. Mirrors the fixture-driven C++
//! tests (which need IndexDriver); this exercises NameBuilder + TuParser +
//! AstWalker + FileRegistry + BatchSink directly.

use symhow::index::ast_walker::AstWalker;
use symhow::index::file_registry::{FileRegistry, FileRegistryOptions};
use symhow::index::tu_parser::parse_tu;
use symhow::model::{BatchSink, SymbolKind};

fn name_path(n: &symhow::model::Name) -> String {
    n.elements
        .iter()
        .map(|e| e.name.clone())
        .collect::<Vec<_>>()
        .join(&n.delimiter)
}

fn temp_source() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("symhow_astwalker_smoke_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("smoke.cpp");
    std::fs::write(
        &src,
        "namespace ns {\nstruct Widget { int x; };\nint helper(int v) { return v * 2; }\n}\nint main() { ns::Widget w; return ns::helper(w.x); }\n",
    )
    .unwrap();
    src
}

#[test]
fn parse_and_walk_emits_expected_facts() {
    let src = temp_source();
    let src_path = src.to_str().unwrap().to_string();

    // Parse: sanitized-argv style, excluding argv[0].
    let args = vec![
        "-x".to_string(),
        "c++".to_string(),
        "-std=c++17".to_string(),
        src_path.clone(),
    ];
    let parsed = parse_tu(&args);
    assert!(parsed.ok, "TU failed to parse: {:?}", parsed.diagnostics);
    assert!(
        parsed.diagnostics.iter().all(|d| !d.fatal),
        "fatal diagnostics: {:?}",
        parsed.diagnostics
    );

    // Walk into a BatchSink.
    let mut files = FileRegistry::new(FileRegistryOptions::default());
    let mut sink = BatchSink::default();
    {
        let mut walker = AstWalker::new(parsed.tu.get(), &mut files, &mut sink);
        walker.run();
    }

    let batch = sink.batch();

    // The main file must be registered as indexed.
    assert!(batch.files.iter().any(|f| f.path.ends_with("smoke.cpp") && f.indexed));

    // Symbols: ns (namespace), ns::Widget (struct), Widget::x (field),
    // ns::helper (function), main (function) -- plus the file symbol.
    let names: Vec<(String, SymbolKind)> = batch
        .symbols
        .iter()
        .map(|s| (name_path(&s.name), s.kind))
        .collect();

    assert!(names.iter().any(|(n, _)| n == "ns"), "missing namespace ns: {:?}", names);
    assert!(names.iter().any(|(n, _)| n == "ns::Widget"), "missing ns::Widget: {:?}", names);
    assert!(names.iter().any(|(n, _)| n == "ns::helper"), "missing ns::helper: {:?}", names);
    assert!(names.iter().any(|(n, _)| n == "main"), "missing main: {:?}", names);

    let helper = batch
        .symbols
        .iter()
        .find(|s| name_path(&s.name) == "ns::helper")
        .expect("helper symbol");
    assert_eq!(helper.kind, SymbolKind::Function);
    // helper's qualified name carries its signature postfix.
    assert!(helper.name.elements.last().unwrap().postfix.starts_with("(int)"));

    // References: main -> ns::helper (CALL), main -> ns::Widget (TYPE_USAGE).
    assert!(!batch.references.is_empty(), "no references emitted");
    assert!(
        batch.occurrences.iter().any(|o| !o.range.file.to_string().is_empty()),
        "no occurrences with a resolved file"
    );
    assert!(!batch.occurrences.is_empty());

    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

