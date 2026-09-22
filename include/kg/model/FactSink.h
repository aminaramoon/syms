#pragma once

#include <string>
#include <unordered_map>
#include <unordered_set>

#include "kg/model/Facts.h"

namespace kg::model
{

// Abstract sink for facts produced while walking a single translation
// unit's AST. Implementations own the dedup policy documented per method
// below. kg::index::AstWalker only ever talks to a FactSink -- never
// directly to a TuFactBatch or a backend -- so extraction code stays
// backend-agnostic.
class FactSink
{
public:
    virtual ~FactSink() = default;

    // Dedups by path. Returns the (stable, per-sink) LocalId for `f.path`.
    virtual LocalId file(FileFact f) = 0;

    // Dedups by key. On a new key, stores `s` verbatim and returns a fresh
    // LocalId. On a known key, refines the existing fact's kind/definition
    // upward (see refineSymbol) and returns the existing LocalId; the new
    // fact's `name` is otherwise ignored (NameBuilder is deterministic per
    // key, so it should already match).
    virtual LocalId symbol(SymbolFact s) = 0;

    // Dedups by name. Returns the (stable, per-sink) LocalId for `uniqueName`.
    virtual LocalId localSymbol(std::string uniqueName) = 0;

    // Dedups by the (ctx, target, kind) triple. Returns the (stable,
    // per-sink) LocalId of the resulting ReferenceFact.
    virtual LocalId reference(LocalId ctx, LocalId target, ReferenceKind k) = 0;

    // Never deduped: always appends a fresh ReferenceFact with
    // target=kNoLocal and returns its LocalId.
    virtual LocalId unsolvedReference(LocalId ctx, ReferenceKind k) = 0;

    // Dedups by the full (cls, element, range, kind) tuple; a duplicate
    // occurrence is silently dropped.
    virtual void occurrence(ElementClass cls, LocalId element, SourceRange range, LocationKind kind) = 0;

    // Never deduped: always appends a fresh ErrorFact.
    virtual void error(std::string msg, bool fatal, const SourceRange* r) = 0;

    // Upgrade-only refinement of an already-recorded symbol: `kind` is
    // adopted only if kindRank(kind) > kindRank(current); `definition` is
    // adopted only if it ranks higher (Explicit > Implicit > None) than the
    // current one.
    virtual void refineSymbol(LocalId id, SymbolKind kind, DefinitionKind definition) = 0;

    // Records that `symbol` was declared/defined in `file` (both LocalIds,
    // into `symbols`/`files` respectively) in this TU. Deduped: recording
    // the same (symbol, file) pair twice is a no-op. No-op if `symbol` is
    // kNoLocal or otherwise unknown to this sink.
    virtual void declaredIn(LocalId symbol, LocalId file) = 0;
};

// Default FactSink implementation: buffers everything into a TuFactBatch,
// applying the dedup rules documented on FactSink above.
class BatchSink final : public FactSink
{
public:
    LocalId file(FileFact f) override;
    LocalId symbol(SymbolFact s) override;
    LocalId localSymbol(std::string uniqueName) override;
    LocalId reference(LocalId ctx, LocalId target, ReferenceKind k) override;
    LocalId unsolvedReference(LocalId ctx, ReferenceKind k) override;
    void occurrence(ElementClass cls, LocalId element, SourceRange range, LocationKind kind) override;
    void error(std::string msg, bool fatal, const SourceRange* r) override;
    void refineSymbol(LocalId id, SymbolKind kind, DefinitionKind definition) override;
    void declaredIn(LocalId symbol, LocalId file) override;

    const TuFactBatch& batch() const { return m_b; }
    TuFactBatch take() { return std::move(m_b); }

private:
    TuFactBatch m_b;
    std::unordered_map<std::string, LocalId> m_byPath;
    std::unordered_map<std::string, LocalId> m_byKey;
    std::unordered_map<std::string, LocalId> m_byLocalName;
    std::unordered_map<uint64_t, LocalId> m_byRefTriple;
    std::unordered_set<uint64_t> m_occurrenceSeen;
    std::unordered_set<uint64_t> m_declaredInSeen;
};

}  // namespace kg::model
