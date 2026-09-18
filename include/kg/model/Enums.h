#pragma once

#include <cstdint>

// Backend-neutral fact-extraction enums. Deliberately NOT sourcetrail::
// types: this header (and everything in kg_model / the AST-walking code) is
// not allowed to depend on third_party/sourcetraildb, so a future
// non-libclang backend (e.g. CodeQL) could plug into the same
// FactSink/IndexBackend seam without dragging in the vendored writer.
// Translation to sourcetrail::SymbolKind/ReferenceKind lives ONLY in
// kg::index::SrctrldbBackend (src/index/SrctrldbBackend.cpp).
namespace kg::model
{

enum class SymbolKind : uint8_t
{
    Unknown,
    Type,
    Namespace,
    Struct,
    Class,
    Union,
    Enum,
    EnumConstant,
    Field,
    GlobalVariable,
    Function,
    Method,
    Typedef,
    Macro,
    File,
};

enum class ReferenceKind : uint8_t
{
    Unknown,
    TypeUsage,
    Usage,
    Call,
    Inheritance,
    Override,
    Include,
    MacroUsage,
};

enum class DefinitionKind : uint8_t
{
    None = 0,
    Implicit = 1,
    Explicit = 2,
};

enum class LocationKind : uint8_t
{
    Token,
    Scope,
    Qualifier,
    LocalSymbol,
    Signature,
    IndexerError,
    Unsolved,
};

enum class ElementClass : uint8_t
{
    Symbol,
    Reference,
    LocalSymbol,
};

// Upgrade-only refinement lattice used by FactSink::symbol()/refineSymbol()
// to decide whether a newly-observed SymbolKind for an already-known symbol
// key should replace the previously recorded one: Unknown=0, Type=1,
// everything else=2 (all "real" kinds are considered equally more precise
// than the Type/Unknown placeholders; once a symbol is known to be e.g. a
// Function, a later observation of it as a bare Type fallback must never
// downgrade it back).
int kindRank(SymbolKind kind);

}  // namespace kg::model
