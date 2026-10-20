//! Backend-neutral fact types. Mirrors `symhow::model::Facts`.

use super::enums::{DefinitionKind, ElementClass, LocationKind, ReferenceKind, SymbolKind};

/// An index into one of `TuFactBatch`'s per-kind arrays.
pub type LocalId = u32;
pub const K_NO_LOCAL: LocalId = 0xFFFF_FFFF;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NameElement {
    pub prefix: String,
    pub name: String,
    pub postfix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Name {
    pub delimiter: String,
    pub elements: Vec<NameElement>,
}

impl Name {
    pub fn new(delimiter: impl Into<String>) -> Name {
        Name { delimiter: delimiter.into(), elements: Vec::new() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceRange {
    pub file: LocalId,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl SourceRange {
    pub fn new(file: LocalId, start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> SourceRange {
        SourceRange { file, start_line, start_col, end_line, end_col }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileFact {
    pub path: String,
    pub language: String,
    pub indexed: bool,
    pub system: bool,
    /// SHA-256 of libclang's exact parsed bytes; empty means unavailable.
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SymbolFact {
    pub key: String,
    pub name: Name,
    pub kind: SymbolKind,
    pub definition: DefinitionKind,
    /// Files (LocalIds into `TuFactBatch::files`) in which this symbol was
    /// seen declared or defined in THIS translation unit.
    pub decl_files: Vec<LocalId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReferenceFact {
    pub context: LocalId,
    pub target: LocalId,
    pub kind: ReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OccurrenceFact {
    pub cls: ElementClass,
    pub element: LocalId,
    pub range: SourceRange,
    pub kind: LocationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ErrorFact {
    pub message: String,
    pub fatal: bool,
    pub range: SourceRange,
    pub has_range: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TuStats {
    pub tu_file: String,
    /// 0=not CUDA/first try, 1=Full, 2=NoWrapper, 3=HostOnly, 4=all failed
    pub parsed_rung_index: u32,
    pub diag_errors: u32,
    pub diag_fatal: u32,
    pub parse_ms: u64,
    pub walk_ms: u64,
    pub parsed_ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TuFactBatch {
    pub stats: TuStats,
    pub files: Vec<FileFact>,
    pub symbols: Vec<SymbolFact>,
    pub local_symbols: Vec<String>,
    pub references: Vec<ReferenceFact>,
    pub occurrences: Vec<OccurrenceFact>,
    pub errors: Vec<ErrorFact>,
}

impl TuFactBatch {
    pub fn has_content(&self) -> bool {
        !self.symbols.is_empty() || !self.references.is_empty()
    }
}