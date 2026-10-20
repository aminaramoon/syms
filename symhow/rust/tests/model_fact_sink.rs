//! Model layer coverage (BatchSink dedup + enum refinement lattice). The
//! C++ suite exercises these indirectly via test_kind_determinism.cpp (an
//! index-layer integration test); this pins them directly for the Rust port.

use symhow::model::enums::{kind_rank, symbol_kind_beats, DefinitionKind, ElementClass, LocationKind, ReferenceKind, SymbolKind};
use symhow::model::{BatchSink, FactSink, FileFact, Name, NameElement, SourceRange, SymbolFact, K_NO_LOCAL};

#[test]
fn kind_rank_lattice() {
    assert_eq!(kind_rank(SymbolKind::Unknown), 0);
    assert_eq!(kind_rank(SymbolKind::Type), 1);
    assert_eq!(kind_rank(SymbolKind::Namespace), 2);
    assert_eq!(kind_rank(SymbolKind::Class), 2);
    assert_eq!(kind_rank(SymbolKind::Function), 2);
    assert_eq!(kind_rank(SymbolKind::File), 2);
}

#[test]
fn symbol_kind_beats_rank_then_discriminant() {
    // Higher rank beats lower.
    assert!(symbol_kind_beats(SymbolKind::Type, SymbolKind::Unknown));
    assert!(symbol_kind_beats(SymbolKind::Class, SymbolKind::Type));
    assert!(!symbol_kind_beats(SymbolKind::Unknown, SymbolKind::Class));

    // Equal rank: higher discriminant wins (order-independent).
    assert!(symbol_kind_beats(SymbolKind::Class, SymbolKind::Struct));
    assert!(!symbol_kind_beats(SymbolKind::Struct, SymbolKind::Class));
    assert!(symbol_kind_beats(SymbolKind::Method, SymbolKind::Function));
}

fn name(delimiter: &str, elements: Vec<NameElement>) -> Name {
    Name { delimiter: delimiter.to_string(), elements }
}

fn symbol(key: &str, kind: SymbolKind, definition: DefinitionKind) -> SymbolFact {
    SymbolFact {
        key: key.to_string(),
        name: name("::", vec![NameElement { prefix: String::new(), name: key.to_string(), postfix: String::new() }]),
        kind,
        definition,
        decl_files: Vec::new(),
    }
}

#[test]
fn batch_sink_file_dedup_and_merge() {
    let mut sink = BatchSink::default();
    let a = sink.file(FileFact { path: "a.cpp".into(), language: "cpp".into(), indexed: true, system: false, content_hash: String::new() });
    let b = sink.file(FileFact { path: "a.cpp".into(), language: String::new(), indexed: false, system: false, content_hash: "hash".into() });
    assert_eq!(a, b);
    assert_eq!(sink.batch().files.len(), 1);
    let f = &sink.batch().files[0];
    assert_eq!(f.language, "cpp"); // first-write-wins
    assert_eq!(f.content_hash, "hash"); // fills in empty
    assert!(f.indexed); // OR
    assert!(!f.system); // AND
}

#[test]
fn batch_sink_symbol_dedup_upgrades_kind_and_definition() {
    let mut sink = BatchSink::default();
    let a = sink.symbol(symbol("foo", SymbolKind::Unknown, DefinitionKind::None));
    let b = sink.symbol(symbol("foo", SymbolKind::Class, DefinitionKind::Explicit));
    assert_eq!(a, b);
    assert_eq!(sink.batch().symbols.len(), 1);
    let s = &sink.batch().symbols[0];
    assert_eq!(s.kind, SymbolKind::Class);
    assert_eq!(s.definition, DefinitionKind::Explicit);

    // A later, weaker observation must not downgrade.
    let c = sink.symbol(symbol("foo", SymbolKind::Type, DefinitionKind::Implicit));
    assert_eq!(c, a);
    assert_eq!(sink.batch().symbols[0].kind, SymbolKind::Class);
    assert_eq!(sink.batch().symbols[0].definition, DefinitionKind::Explicit);
}

#[test]
fn batch_sink_local_symbol_and_reference_dedup() {
    let mut sink = BatchSink::default();
    let l1 = sink.local_symbol("loc".into());
    let l2 = sink.local_symbol("loc".into());
    assert_eq!(l1, l2);
    assert_eq!(sink.batch().local_symbols.len(), 1);

    let s0 = sink.symbol(symbol("a", SymbolKind::Function, DefinitionKind::Explicit));
    let s1 = sink.symbol(symbol("b", SymbolKind::Function, DefinitionKind::Explicit));
    let r1 = sink.reference(s0, s1, ReferenceKind::Call);
    let r2 = sink.reference(s0, s1, ReferenceKind::Call);
    assert_eq!(r1, r2);
    assert_eq!(sink.batch().references.len(), 1);
}

#[test]
fn batch_sink_unsolved_reference_never_deduped() {
    let mut sink = BatchSink::default();
    let s0 = sink.symbol(symbol("a", SymbolKind::Function, DefinitionKind::Explicit));
    let r1 = sink.unsolved_reference(s0, ReferenceKind::Usage);
    let r2 = sink.unsolved_reference(s0, ReferenceKind::Usage);
    assert_ne!(r1, r2);
    assert_eq!(sink.batch().references.len(), 2);
    assert_eq!(sink.batch().references[0].target, K_NO_LOCAL);
    assert_eq!(sink.batch().references[1].target, K_NO_LOCAL);
}

#[test]
fn batch_sink_occurrence_dedup_by_full_tuple() {
    let mut sink = BatchSink::default();
    let s0 = sink.symbol(symbol("a", SymbolKind::Function, DefinitionKind::Explicit));
    let range = SourceRange::new(0, 1, 2, 3, 4);
    sink.occurrence(ElementClass::Symbol, s0, range, LocationKind::Token);
    sink.occurrence(ElementClass::Symbol, s0, range, LocationKind::Token); // dup
    sink.occurrence(ElementClass::Symbol, s0, range, LocationKind::Scope); // distinct kind
    assert_eq!(sink.batch().occurrences.len(), 2);
}

#[test]
fn batch_sink_error_never_deduped_and_declared_in_deduped() {
    let mut sink = BatchSink::default();
    let s0 = sink.symbol(symbol("a", SymbolKind::Function, DefinitionKind::Explicit));
    let f0 = sink.file(FileFact { path: "a.cpp".into(), language: "cpp".into(), indexed: true, system: false, content_hash: String::new() });

    sink.error("boom".into(), false, None);
    sink.error("boom".into(), false, None);
    assert_eq!(sink.batch().errors.len(), 2);

    sink.declared_in(s0, f0);
    sink.declared_in(s0, f0); // dup
    assert_eq!(sink.batch().symbols[0].decl_files.len(), 1);
    assert_eq!(sink.batch().symbols[0].decl_files[0], f0);
}