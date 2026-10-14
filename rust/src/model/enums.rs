//! Backend-neutral enums. Mirrors `symhow::model::Enums`.

#[derive(Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum SymbolKind {
    #[default]
    Unknown = 0,
    Type = 1,
    Namespace = 2,
    Struct = 3,
    Class = 4,
    Union = 5,
    Enum = 6,
    EnumConstant = 7,
    Field = 8,
    GlobalVariable = 9,
    Function = 10,
    Method = 11,
    Typedef = 12,
    Macro = 13,
    File = 14,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum ReferenceKind {
    #[default]
    Unknown = 0,
    TypeUsage = 1,
    Usage = 2,
    Call = 3,
    Inheritance = 4,
    Override = 5,
    Include = 6,
    MacroUsage = 7,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum DefinitionKind {
    #[default]
    None = 0,
    Implicit = 1,
    Explicit = 2,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum LocationKind {
    #[default]
    Token = 0,
    Scope = 1,
    Qualifier = 2,
    LocalSymbol = 3,
    Signature = 4,
    IndexerError = 5,
    Unsolved = 6,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum ElementClass {
    #[default]
    Symbol = 0,
    Reference = 1,
    LocalSymbol = 2,
}

/// Upgrade-only refinement lattice: Unknown=0, Type=1, everything else=2.
pub fn kind_rank(kind: SymbolKind) -> i32 {
    match kind {
        SymbolKind::Unknown => 0,
        SymbolKind::Type => 1,
        _ => 2,
    }
}

/// Deterministic, order-independent "does `candidate` strictly beat
/// `current`" comparison.
pub fn symbol_kind_beats(candidate: SymbolKind, current: SymbolKind) -> bool {
    let candidate_rank = kind_rank(candidate);
    let current_rank = kind_rank(current);
    if candidate_rank != current_rank {
        return candidate_rank > current_rank;
    }
    (candidate as u8) > (current as u8)
}