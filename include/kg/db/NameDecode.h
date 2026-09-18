#pragma once

#include <string>
#include <vector>

// Inverse of sourcetrail::serializeNameHierarchyToDatabaseString() (see
// third_party/sourcetraildb/src/NameHierarchy.cpp), which is what `node.
// serialized_name` actually contains on disk. This header does NOT depend on
// third_party/sourcetraildb -- it re-implements the (tiny, stable-by-schema-
// version) inverse directly against the delimiter constants, since kg_db must
// stay a read-only consumer of the on-disk format, not a client of the writer
// library.
namespace kg::db
{

struct DecodedNameElement
{
    std::string prefix;
    std::string name;
    std::string postfix;
};

// A fully-decoded `node.serialized_name`. See NameDecode.cpp for the exact
// delimiter grammar (verified against the vendored source, not guessed).
struct DecodedName
{
    std::string nameDelimiter;
    std::vector<DecodedNameElement> elements;

    // Pure "A::B::C" path: elements' `.name` fields only, joined by
    // nameDelimiter. Never includes any element's prefix/postfix (so, unlike
    // qualifiedName()/displayName() below, two overloads of the same
    // function collapse to the same namePath -- this is intentional: it's
    // what SymbolResolver uses for plain "give me the thing named X" lookups
    // where the caller hasn't specified a signature).
    std::string namePath() const;

    // The "real" identity string: namePath() with the LAST element's postfix
    // appended verbatim (no separator). For non-function symbols (namespace/
    // class/field/...) postfix is always empty so this equals namePath(). For
    // functions/methods it includes the parameter-list postfix (so overloads
    // get distinct qualifiedName()s), and -- crucially -- it also includes
    // Phase 3's hidden TU-local disambiguator suffix when present (e.g.
    // " [tu_a.cpp#317677a3]"), since that disambiguator is itself stored as
    // part of the last element's postfix (see NameBuilder.cpp
    // identityElement()). This is what round-trips exactly: feeding a node's
    // own qualifiedName() back into SymbolResolver::resolveSymbol() always
    // resolves uniquely to that exact node, even across a USR collision that
    // collapsed two TUs' same-named statics onto textually-identical
    // namePath()s.
    std::string qualifiedName() const;

    // qualifiedName() with the trailing disambiguator suffix (if present)
    // stripped, for human-friendly display. Only strips a suffix that
    // matches the EXACT pattern NameBuilder.cpp's disambiguatorFor()
    // produces -- ` [<basename>#<8 lowercase hex digits>]` at the very end
    // of the string -- so a legitimate postfix that happens to end in
    // brackets (unlikely in this fixture, but e.g. an array-type spelling)
    // is never mistakenly truncated.
    std::string displayName() const;

    // Last element's `prefix + " " + name + postfix`, e.g.
    // "std::string get_policy_name() const". Useful for disambiguating
    // overloads beyond what qualifiedName() alone captures (return type is
    // NOT part of qualifiedName(), but is part of signature()).
    std::string signature() const;
};

DecodedName decodeSerializedName(const std::string& serialized);

}  // namespace kg::db
