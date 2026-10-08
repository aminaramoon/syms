#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "kg/model/Enums.h"

// Backend-neutral fact types produced by extraction (e.g. kg::index's
// libclang AST walker) and consumed by an IndexBackend (e.g.
// kg::index::SrctrldbBackend). See FactSink.h / IndexBackend.h for the
// seam this data model plugs into.
namespace kg::model
{

// An index into one of TuFactBatch's per-kind arrays (files, symbols,
// localSymbols, references). Each array has its own independent id space
// starting at 0; which array a given LocalId indexes into is always
// unambiguous from context (e.g. ReferenceFact::context/target always index
// into `symbols`, SourceRange::file always indexes into `files`,
// OccurrenceFact pairs an ElementClass with the LocalId to say which array
// `element` indexes into).
using LocalId = uint32_t;
inline constexpr LocalId kNoLocal = 0xFFFFFFFFu;

struct NameElement
{
    std::string prefix;
    std::string name;
    std::string postfix;
};

struct Name
{
    std::string delimiter = "::";
    std::vector<NameElement> elements;
};

struct SourceRange
{
    LocalId file = kNoLocal;
    uint32_t startLine{};
    uint32_t startCol{};
    uint32_t endLine{};
    uint32_t endCol{};
};

struct FileFact
{
    std::string path;
    std::string language;
    bool indexed = false;
    bool system = false;
    // SHA-256 of libclang's exact parsed bytes; empty means unavailable.
    std::string contentHash;
};

struct SymbolFact
{
    std::string key;
    Name name;
    SymbolKind kind = SymbolKind::Unknown;
    DefinitionKind definition = DefinitionKind::None;

    // Files (LocalIds into TuFactBatch::files) in which this symbol was seen
    // declared or defined in THIS translation unit -- i.e. every file whose
    // visited decl/definition cursor caused ensureSymbol()/registerOne() to
    // register this symbol (not files that merely reference it). Populated
    // via FactSink::declaredIn(); deduped by BatchSink. Consumed by
    // SrctrldbBackend to populate the kg_symbol_def side table (see
    // docs/KNOWN_LIMITATIONS.md's Phase 1 section).
    std::vector<LocalId> declFiles;
};

// A reference edge between two symbols (both `context` and `target` index
// into TuFactBatch::symbols). For an "unsolved" reference (no resolvable
// target), `target` is kNoLocal and the backend must route the fact through
// sourcetrail::SourcetrailDBWriter::recordReferenceToUnsolvedSymhol instead
// of recordReference.
struct ReferenceFact
{
    LocalId context = kNoLocal;
    LocalId target = kNoLocal;
    ReferenceKind kind = ReferenceKind::Unknown;
};

struct OccurrenceFact
{
    ElementClass cls{};
    LocalId element = kNoLocal;
    SourceRange range;
    LocationKind kind{};
};

struct ErrorFact
{
    std::string message;
    bool fatal = false;
    SourceRange range;
    bool hasRange = false;
};

struct TuStats
{
    std::string tuFile;
    uint32_t parsedRungIndex = 0;  // 0=not CUDA/first try, 1=Full, 2=NoWrapper, 3=HostOnly, 4=all failed
    uint32_t diagErrors = 0;
    uint32_t diagFatal = 0;
    uint64_t parseMs = 0;
    uint64_t walkMs = 0;
    bool parsedOk = false;
};

struct TuFactBatch
{
    TuStats stats;
    std::vector<FileFact> files;
    std::vector<SymbolFact> symbols;
    std::vector<std::string> localSymbols;
    std::vector<ReferenceFact> references;
    std::vector<OccurrenceFact> occurrences;
    std::vector<ErrorFact> errors;

    bool hasContent() const { return !symbols.empty() || !references.empty(); }
};

}  // namespace kg::model
