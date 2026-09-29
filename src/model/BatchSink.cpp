#include "kg/model/FactSink.h"

#include <utility>

namespace kg::model
{

namespace
{

// boost::hash_combine-style mixer; used only for in-memory, per-TU dedup
// sets/maps (not persisted anywhere), so 64-bit collision odds are a total
// non-issue at TU scale.
void hashCombine(uint64_t& seed, uint64_t value)
{
    seed ^= value + 0x9e3779b97f4a7c15ull + (seed << 6) + (seed >> 2);
}

uint64_t refTripleKey(LocalId ctx, LocalId target, ReferenceKind k)
{
    uint64_t seed = 0;
    hashCombine(seed, ctx);
    hashCombine(seed, target);
    hashCombine(seed, static_cast<uint64_t>(k));
    return seed;
}

uint64_t occurrenceKey(ElementClass cls, LocalId element, const SourceRange& range, LocationKind kind)
{
    uint64_t seed = 0;
    hashCombine(seed, static_cast<uint64_t>(cls));
    hashCombine(seed, element);
    hashCombine(seed, range.file);
    hashCombine(seed, range.startLine);
    hashCombine(seed, range.startCol);
    hashCombine(seed, range.endLine);
    hashCombine(seed, range.endCol);
    hashCombine(seed, static_cast<uint64_t>(kind));
    return seed;
}

// Upgrade-only merge used both by symbol() (dedup on a known key) and by
// refineSymbol().
void upgrade(SymbolFact& existing, SymbolKind kind, DefinitionKind definition)
{
    // symbolKindBeats() (not a bare kindRank() > comparison) so an
    // equal-rank disagreement within one TU is resolved the same
    // deterministic, order-independent way as the cross-TU/cross-session
    // merge in kg::index::SrctrldbBackend::bufferKindDef() -- see its
    // comment for the corpus bug this class of "first observed wins" tie-
    // break caused.
    if (symbolKindBeats(kind, existing.kind))
    {
        existing.kind = kind;
    }
    if (static_cast<int>(definition) > static_cast<int>(existing.definition))
    {
        existing.definition = definition;
    }
}

}  // namespace

LocalId BatchSink::file(FileFact f)
{
    auto it = m_byPath.find(f.path);
    if (it != m_byPath.end())
    {
        // First-write-wins for content, but let a later, more-informative
        // call fill in fields the first call left at their defaults (these
        // should never actually disagree in practice: FileRegistry computes
        // indexed/system once per path per TU).
        FileFact& existing = m_b.files[it->second];
        if (existing.language.empty())
        {
            existing.language = f.language;
        }
        existing.indexed = existing.indexed || f.indexed;
        existing.system = existing.system && f.system;
        return it->second;
    }

    const LocalId id = static_cast<LocalId>(m_b.files.size());
    m_b.files.push_back(std::move(f));
    m_byPath.emplace(m_b.files.back().path, id);
    return id;
}

LocalId BatchSink::symbol(SymbolFact s)
{
    auto it = m_byKey.find(s.key);
    if (it != m_byKey.end())
    {
        upgrade(m_b.symbols[it->second], s.kind, s.definition);
        return it->second;
    }

    const LocalId id = static_cast<LocalId>(m_b.symbols.size());
    m_byKey.emplace(s.key, id);
    m_b.symbols.push_back(std::move(s));
    return id;
}

LocalId BatchSink::localSymbol(std::string uniqueName)
{
    auto it = m_byLocalName.find(uniqueName);
    if (it != m_byLocalName.end())
    {
        return it->second;
    }

    const LocalId id = static_cast<LocalId>(m_b.localSymbols.size());
    m_byLocalName.emplace(uniqueName, id);
    m_b.localSymbols.push_back(std::move(uniqueName));
    return id;
}

LocalId BatchSink::reference(LocalId ctx, LocalId target, ReferenceKind k)
{
    const uint64_t key = refTripleKey(ctx, target, k);
    auto it = m_byRefTriple.find(key);
    if (it != m_byRefTriple.end())
    {
        return it->second;
    }

    const LocalId id = static_cast<LocalId>(m_b.references.size());
    m_b.references.push_back(ReferenceFact{ctx, target, k});
    m_byRefTriple.emplace(key, id);
    return id;
}

LocalId BatchSink::unsolvedReference(LocalId ctx, ReferenceKind k)
{
    const LocalId id = static_cast<LocalId>(m_b.references.size());
    m_b.references.push_back(ReferenceFact{ctx, kNoLocal, k});
    return id;
}

void BatchSink::occurrence(ElementClass cls, LocalId element, SourceRange range, LocationKind kind)
{
    const uint64_t key = occurrenceKey(cls, element, range, kind);
    if (!m_occurrenceSeen.insert(key).second)
    {
        return;
    }
    m_b.occurrences.push_back(OccurrenceFact{cls, element, range, kind});
}

void BatchSink::error(std::string msg, bool fatal, const SourceRange* r)
{
    ErrorFact e;
    e.message = std::move(msg);
    e.fatal = fatal;
    e.hasRange = (r != nullptr);
    if (r != nullptr)
    {
        e.range = *r;
    }
    m_b.errors.push_back(std::move(e));
}

void BatchSink::refineSymbol(LocalId id, SymbolKind kind, DefinitionKind definition)
{
    if (id >= m_b.symbols.size())
    {
        return;
    }
    upgrade(m_b.symbols[id], kind, definition);
}

void BatchSink::declaredIn(LocalId symbol, LocalId file)
{
    if (symbol == kNoLocal || file == kNoLocal || symbol >= m_b.symbols.size())
    {
        return;
    }
    uint64_t key = 0;
    hashCombine(key, symbol);
    hashCombine(key, file);
    if (!m_declaredInSeen.insert(key).second)
    {
        return;
    }
    m_b.symbols[symbol].declFiles.push_back(file);
}

}  // namespace kg::model
