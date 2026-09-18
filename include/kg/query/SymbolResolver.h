#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "kg/db/Database.h"

namespace kg::query
{

enum class ResolveOutcome
{
    Unique,
    Ambiguous,
    NotFound,
};

struct ResolvedSymbol
{
    int64_t id = 0;
    int kind = 0;
    std::string serializedName;
};

struct ResolveResult
{
    ResolveOutcome outcome = ResolveOutcome::NotFound;
    std::vector<ResolvedSymbol> candidates;  // empty for NotFound; 1 for Unique; 2+ for Ambiguous
};

// Resolves a user-typed `spec` to one or more `node` rows.
//
// If `spec` starts with '#', the rest is parsed as a literal node id (base
// 10); Unique if it exists in `node`, else NotFound.
//
// Otherwise, a candidate `node` row is considered a match against `spec` if
// ANY of these (computed from decodeSerializedName(candidate.serialized_name))
// exactly equals `spec`:
//   - qualifiedName(): the full round-trip identity string, INCLUDING any
//     hidden Phase-3 TU-local disambiguator -- this is what lets a spec
//     copy-pasted from `search`/`stats` output resolve a USR-collision
//     candidate (e.g. two different TUs' same-named `static` `helper()`)
//     unambiguously, without the caller needing to know the disambiguator
//     syntax exists.
//   - displayName(): qualifiedName() with that disambiguator stripped.
//   - namePath(): the bare "A::B::C" path with no signature/postfix at all
//     (so e.g. an overload's parameter list doesn't need to be typed).
//   - the leaf element's bare name alone (no ancestors, no postfix) -- this
//     is what makes a plain, unqualified `kg query bases Square` work
//     without spelling out the enclosing namespace; note this CAN be
//     legitimately ambiguous (e.g. a class `Square` and its same-named
//     constructor both have leaf name "Square" -- that's reported as
//     Ambiguous, not silently resolved, exactly as a class/constructor name
//     collision should be).
//
// Candidates are pre-filtered via a cheap `serialized_name LIKE '%<key>%'`
// scan before being decoded and compared -- purely a performance
// optimization, doesn't affect which candidates are considered exact
// matches. `<key>` is the leading identifier-shaped run (letters/digits/'_'/
// '~') of spec's last "::"-separated segment, e.g. "area" for
// "fixture::Square::area() const" or "helper" for
// "helper() [tu_a.cpp#317677a3]" -- trimmed at the first non-identifier
// character since only an element's bare `.name` is guaranteed to appear as
// a literal contiguous substring of the raw tab-delimited serialized_name;
// a full qualifiedName()/signature() string with its postfix glued on is
// not.
//
// If `kindFilter` is non-null and non-empty, candidates are narrowed to
// those node.type values first. If `signatureFilter` is non-null, candidates
// are further narrowed to those whose decodeSerializedName(...).signature()
// exactly equals *signatureFilter.
//
// Zero matches -> NotFound. One -> Unique. More than one -> Ambiguous (all
// listed, for the caller to print a disambiguation table).
ResolveResult resolveSymbol(kg::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter, const std::string* signatureFilter);

// Same Unique/Ambiguous/NotFound contract as resolveSymbol(), but against the
// `file` table's `path` column instead of decoded symbol names: exact
// `path = spec` match first; if that finds nothing, retries with
// `path LIKE '%' || spec` (so a caller can pass a path suffix, e.g. just
// "shapes.hpp" instead of the full absolute path), and more than one hit
// there is Ambiguous. ResolvedSymbol::kind is always kg::db::NodeKind::FILE;
// ResolvedSymbol::serializedName holds the file's `path` (files don't have a
// serialized_name).
ResolveResult resolveFilePath(kg::db::Database& db, const std::string& spec);

}  // namespace kg::query
