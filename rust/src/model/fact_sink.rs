//! The extraction<->persistence seam. Mirrors `symhow::model::FactSink` and
//! `symhow::model::IndexBackend`.

use std::collections::{HashMap, HashSet};

use crate::common::Result;

use super::enums::{DefinitionKind, ElementClass, LocationKind, ReferenceKind, SymbolKind};
use super::enums::symbol_kind_beats;
use super::facts::{
    FileFact, K_NO_LOCAL, LocalId, OccurrenceFact, ReferenceFact, SourceRange, SymbolFact,
    TuFactBatch,
};

/// Abstract sink for facts produced while walking a single TU's AST.
pub trait FactSink {
    fn file(&mut self, f: FileFact) -> LocalId;
    fn symbol(&mut self, s: SymbolFact) -> LocalId;
    fn local_symbol(&mut self, unique_name: String) -> LocalId;
    fn reference(&mut self, ctx: LocalId, target: LocalId, k: ReferenceKind) -> LocalId;
    fn unsolved_reference(&mut self, ctx: LocalId, k: ReferenceKind) -> LocalId;
    fn occurrence(&mut self, cls: ElementClass, element: LocalId, range: SourceRange, kind: LocationKind);
    fn error(&mut self, msg: String, fatal: bool, r: Option<&SourceRange>);
    fn refine_symbol(&mut self, id: LocalId, kind: SymbolKind, definition: DefinitionKind);
    fn declared_in(&mut self, symbol: LocalId, file: LocalId);
}

/// Default `FactSink`: buffers everything into a `TuFactBatch`.
#[derive(Default)]
pub struct BatchSink {
    batch: TuFactBatch,
    by_path: HashMap<String, LocalId>,
    by_key: HashMap<String, LocalId>,
    by_local_name: HashMap<String, LocalId>,
    by_ref_triple: HashMap<u64, LocalId>,
    occurrence_seen: HashSet<u64>,
    declared_in_seen: HashSet<u64>,
}

fn hash_combine(seed: &mut u64, value: u64) {
    *seed ^= value.wrapping_add(0x9e37_79b9_7f4a_7c15).wrapping_add(*seed << 6).wrapping_add(*seed >> 2);
}

fn ref_triple_key(ctx: LocalId, target: LocalId, k: ReferenceKind) -> u64 {
    let mut seed = 0u64;
    hash_combine(&mut seed, ctx as u64);
    hash_combine(&mut seed, target as u64);
    hash_combine(&mut seed, k as u64);
    seed
}

fn occurrence_key(cls: ElementClass, element: LocalId, range: &SourceRange, kind: LocationKind) -> u64 {
    let mut seed = 0u64;
    hash_combine(&mut seed, cls as u64);
    hash_combine(&mut seed, element as u64);
    hash_combine(&mut seed, range.file as u64);
    hash_combine(&mut seed, range.start_line as u64);
    hash_combine(&mut seed, range.start_col as u64);
    hash_combine(&mut seed, range.end_line as u64);
    hash_combine(&mut seed, range.end_col as u64);
    hash_combine(&mut seed, kind as u64);
    seed
}

fn upgrade(existing: &mut SymbolFact, kind: SymbolKind, definition: DefinitionKind) {
    if symbol_kind_beats(kind, existing.kind) {
        existing.kind = kind;
    }
    if (definition as u8) > (existing.definition as u8) {
        existing.definition = definition;
    }
}

impl BatchSink {
    pub fn batch(&self) -> &TuFactBatch {
        &self.batch
    }

    pub fn take(&mut self) -> TuFactBatch {
        std::mem::take(&mut self.batch)
    }
}

impl FactSink for BatchSink {
    fn file(&mut self, f: FileFact) -> LocalId {
        if let Some(&id) = self.by_path.get(&f.path) {
            let existing = &mut self.batch.files[id as usize];
            if existing.language.is_empty() {
                existing.language = f.language;
            }
            if existing.content_hash.is_empty() {
                existing.content_hash = f.content_hash;
            }
            existing.indexed = existing.indexed || f.indexed;
            existing.system = existing.system && f.system;
            return id;
        }

        let id = self.batch.files.len() as LocalId;
        self.by_path.insert(f.path.clone(), id);
        self.batch.files.push(f);
        id
    }

    fn symbol(&mut self, s: SymbolFact) -> LocalId {
        if let Some(&id) = self.by_key.get(&s.key) {
            let existing = &mut self.batch.symbols[id as usize];
            upgrade(existing, s.kind, s.definition);
            return id;
        }

        let id = self.batch.symbols.len() as LocalId;
        self.by_key.insert(s.key.clone(), id);
        self.batch.symbols.push(s);
        id
    }

    fn local_symbol(&mut self, unique_name: String) -> LocalId {
        if let Some(&id) = self.by_local_name.get(&unique_name) {
            return id;
        }
        let id = self.batch.local_symbols.len() as LocalId;
        self.by_local_name.insert(unique_name.clone(), id);
        self.batch.local_symbols.push(unique_name);
        id
    }

    fn reference(&mut self, ctx: LocalId, target: LocalId, k: ReferenceKind) -> LocalId {
        let key = ref_triple_key(ctx, target, k);
        if let Some(&id) = self.by_ref_triple.get(&key) {
            return id;
        }
        let id = self.batch.references.len() as LocalId;
        self.batch.references.push(ReferenceFact { context: ctx, target, kind: k });
        self.by_ref_triple.insert(key, id);
        id
    }

    fn unsolved_reference(&mut self, ctx: LocalId, k: ReferenceKind) -> LocalId {
        let id = self.batch.references.len() as LocalId;
        self.batch.references.push(ReferenceFact { context: ctx, target: K_NO_LOCAL, kind: k });
        id
    }

    fn occurrence(&mut self, cls: ElementClass, element: LocalId, range: SourceRange, kind: LocationKind) {
        let key = occurrence_key(cls, element, &range, kind);
        if !self.occurrence_seen.insert(key) {
            return;
        }
        self.batch.occurrences.push(OccurrenceFact { cls, element, range, kind });
    }

    fn error(&mut self, msg: String, fatal: bool, r: Option<&SourceRange>) {
        let has_range = r.is_some();
        let range = r.copied().unwrap_or_default();
        self.batch.errors.push(super::facts::ErrorFact { message: msg, fatal, range, has_range });
    }

    fn refine_symbol(&mut self, id: LocalId, kind: SymbolKind, definition: DefinitionKind) {
        if (id as usize) >= self.batch.symbols.len() {
            return;
        }
        let existing = &mut self.batch.symbols[id as usize];
        upgrade(existing, kind, definition);
    }

    fn declared_in(&mut self, symbol: LocalId, file: LocalId) {
        if symbol == K_NO_LOCAL || file == K_NO_LOCAL || (symbol as usize) >= self.batch.symbols.len() {
            return;
        }
        let mut key = 0u64;
        hash_combine(&mut key, symbol as u64);
        hash_combine(&mut key, file as u64);
        if !self.declared_in_seen.insert(key) {
            return;
        }
        self.batch.symbols[symbol as usize].decl_files.push(file);
    }
}

/// Summary produced by a full index run.
#[derive(Debug, Clone, Default)]
pub struct IndexSummary {
    pub tus_total: u32,
    pub tus_attempted: u32,
    pub tus_with_content: u32,
    pub tus_failed: u32,
    pub symbols: u32,
    pub references: u32,
    pub files: u32,
    pub errors: u32,
    pub name_collisions: u32,
    pub elapsed_ms: u64,
    pub failed_tus: Vec<String>,
}

/// The extraction<->persistence seam.
pub trait IndexBackend {
    fn begin(&mut self, db_path: &str) -> Result<()>;
    fn consume(&mut self, batch: &TuFactBatch) -> Result<()>;
    fn finish(&mut self, summary: &mut IndexSummary) -> Result<()>;
}