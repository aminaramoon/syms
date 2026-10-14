#pragma once

#include <string>

#include <clang-c/Index.h>

#include "symhow/model/Facts.h"

// Builds backend-neutral symhow::model::Name hierarchies and SymbolKeys from
// libclang cursors. Stateless and per-TU: every call recomputes everything
// from the cursor alone (via clang_getCanonicalCursor()), so identical
// decls produce identical output independent of visitation order -- this
// is what lets multiple TUs (and, in a future parallel indexer, multiple
// worker threads) agree on a symbol's identity without any shared state.
namespace symhow::index
{

struct SymbolIdentity
{
    std::string key;    // SymbolKey, see NameBuilder.cpp
    symhow::model::Name name;
};

// Builds the Name hierarchy for `cursor` (NOT necessarily canonicalized by
// the caller -- buildName() canonicalizes internally): the semantic-parent
// chain (root to leaf, excluding the translation unit) followed by
// `cursor`'s own element. Function/method cursors get a signature-style
// leaf element (return type prefix, comma-joined argument types postfix);
// everything else gets a plain name element (with anonymous
// namespace/struct/union/enum synthesis where applicable).
symhow::model::Name buildName(CXCursor cursor);

// Computes the SymbolKey + Name for `cursor` per the identity-scope
// disambiguation rule: USR alone for globally-linked symbols; USR +
// '\x1f' + canonical-decl-file-path for translation-unit-local symbols
// (CXLinkage_Internal, or a spelling of "main" whose semantic parent is the
// translation unit), with a deterministic, hidden disambiguator appended to
// the last Name element's postfix in the local case so the display name
// stays visibly distinct across TUs too.
SymbolIdentity buildSymbolIdentity(CXCursor cursor);

// Name used for a local symbol (ParmDecl / local VarDecl / template
// parameter): "<canonical decl file path>:<line>:<col>", derived from
// clang_getCanonicalCursor(cursor)'s location -- always globally unique
// (keyed by an absolute file position) and, crucially, re-derivable
// identically from a *reference* to the same decl (see AstWalker's local
// symbol handling).
std::string buildLocalSymbolName(CXCursor cursor);

}  // namespace symhow::index
