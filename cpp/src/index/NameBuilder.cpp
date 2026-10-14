#include "symhow/index/NameBuilder.h"

#include <algorithm>
#include <filesystem>
#include <regex>
#include <unordered_map>
#include <vector>

#include "symhow/common/Paths.h"
#include "symhow/index/ClangUtil.h"

namespace symhow::index
{

namespace
{

using symhow::index::clangutil::fnv1a64;
using symhow::index::clangutil::hex8;
using symhow::index::clangutil::toString;
using symhow::model::Name;
using symhow::model::NameElement;

std::string basenameOf(const std::string& path)
{
    return std::filesystem::path(path).filename().string();
}

// Normalized (symhow::common::paths::normalize()) file path for a CXFile,
// memoized per (worker) thread -- normalize() does real filesystem stat
// calls (weakly_canonical()), so this avoids re-stat'ing the same header
// for every local-scope declaration/lambda it contains.
//
// Real, corpus-observed bug this fixes: every identity/disambiguation
// string below used to embed clangutil::fileName()'s raw, AS-SPELLED-BY-
// THIS-TU path verbatim. The SAME physical header reached via two
// textually different #include spellings (e.g. a relative "../../src/
// data/io_worker.hpp" from one TU vs a canonical "src/data/io_worker.hpp"
// from another) therefore produced two DIFFERENT identity keys/
// disambiguators for the exact same translation-unit-local entity (a
// lambda, a `static` function, an anonymous-namespace member) depending on
// which TU happened to be first to touch it this session -- caught via
// `symhow dbdiff` between a fresh `symhow index` and a resumed/partial `symhow update`
// on the real cucs corpus (a lambda inside `io_worker.hpp`, reached both
// via `test/data/../../src/data/io_worker.hpp` and `src/data/io_worker.hpp`,
// ending up as two distinct nodes). Normalizing first makes the identity a
// pure function of the physical file, independent of how any particular TU
// spelled its #include.
std::string normalizedFileName(CXFile file)
{
    if (file == nullptr)
    {
        return {};
    }
    thread_local std::unordered_map<std::string, std::string> cache;
    const std::string raw = clangutil::fileName(file);
    auto it = cache.find(raw);
    if (it != cache.end())
    {
        return it->second;
    }
    const std::string normalized = symhow::common::paths::normalize(raw);
    cache.emplace(raw, normalized);
    return normalized;
}

// file:line for an anonymous record/enum's synthesized display name.
void cursorFileAndLine(CXCursor c, std::string& file, unsigned& line)
{
    CXSourceLocation loc = clang_getCursorLocation(c);
    CXFile f = nullptr;
    unsigned col = 0, offset = 0;
    clang_getFileLocation(loc, &f, &line, &col, &offset);
    file = normalizedFileName(f);
}

std::string kindWordFor(CXCursorKind kind)
{
    switch (kind)
    {
        case CXCursor_StructDecl:
            return "struct";
        case CXCursor_UnionDecl:
            return "union";
        case CXCursor_EnumDecl:
            return "enum";
        case CXCursor_ClassDecl:
        case CXCursor_ClassTemplate:
        case CXCursor_ClassTemplatePartialSpecialization:
            return "class";
        default:
            return "record";
    }
}

NameElement anonymousRecordElement(CXCursor c)
{
    std::string file;
    unsigned line = 0;
    cursorFileAndLine(c, file, line);
    return NameElement{
        "", "(anonymous " + kindWordFor(clang_getCursorKind(c)) + " at " + basenameOf(file) + ":" + std::to_string(line) + ")", ""};
}

NameElement namespaceElement(CXCursor c)
{
    const std::string spelling = toString(clang_getCursorSpelling(c));
    if (spelling.empty())
    {
        return NameElement{"", "(anonymous namespace)", ""};
    }
    return NameElement{"", spelling, ""};
}

NameElement plainElement(CXCursor c)
{
    return NameElement{"", toString(clang_getCursorSpelling(c)), ""};
}

bool isRecordLike(CXCursorKind kind)
{
    return kind == CXCursor_StructDecl || kind == CXCursor_ClassDecl || kind == CXCursor_UnionDecl ||
           kind == CXCursor_ClassTemplate || kind == CXCursor_ClassTemplatePartialSpecialization;
}

bool isFunctionLike(CXCursorKind kind)
{
    return kind == CXCursor_FunctionDecl || kind == CXCursor_FunctionTemplate || kind == CXCursor_CXXMethod ||
           kind == CXCursor_Constructor || kind == CXCursor_Destructor || kind == CXCursor_ConversionFunction;
}

NameElement functionElement(CXCursor c)
{
    NameElement el;
    const CXCursorKind kind = clang_getCursorKind(c);

    if (kind != CXCursor_Constructor && kind != CXCursor_Destructor)
    {
        const std::string resultSpelling = toString(clang_getTypeSpelling(clang_getCursorResultType(c)));
        if (!resultSpelling.empty())
        {
            el.prefix = resultSpelling + " ";
        }
    }

    el.name = toString(clang_getCursorSpelling(c));

    std::string params = "(";
    // clang-c has no clang_Cursor_getArgumentType(); the argument cursor's
    // own type (via clang_getCursorType()) is the supported equivalent.
    const int numArgs = clang_Cursor_getNumArguments(c);
    for (int i = 0; i < numArgs; ++i)
    {
        if (i != 0)
        {
            params += ", ";
        }
        CXCursor argCursor = clang_Cursor_getArgument(c, static_cast<unsigned>(i));
        params += toString(clang_getTypeSpelling(clang_getCursorType(argCursor)));
    }
    params += ")";

    if (clang_CXXMethod_isConst(c) != 0)
    {
        params += " const";
    }
    const CXRefQualifierKind rq = clang_Type_getCXXRefQualifier(clang_getCursorType(c));
    if (rq == CXRefQualifier_LValue)
    {
        params += " &";
    }
    else if (rq == CXRefQualifier_RValue)
    {
        params += " &&";
    }

    el.postfix = params;
    return el;
}

NameElement elementForCursor(CXCursor c)
{
    const CXCursorKind kind = clang_getCursorKind(c);

    if (kind == CXCursor_Namespace)
    {
        return namespaceElement(c);
    }
    if (isRecordLike(kind) || kind == CXCursor_EnumDecl)
    {
        const std::string spelling = toString(clang_getCursorSpelling(c));
        if (spelling.empty())
        {
            return anonymousRecordElement(c);
        }
        return NameElement{"", spelling, ""};
    }
    if (isFunctionLike(kind))
    {
        return functionElement(c);
    }
    return plainElement(c);
}

// Walks the semantic-parent chain from `cursor` up to (not including) the
// translation unit, root to leaf. Each ancestor is canonicalized before
// being added and before walking further up, so identity/naming is always
// computed from the same canonical cursors NameBuilder would use if asked
// about that ancestor directly.
//
// CXCursor_LinkageSpec (an `extern "C" { ... }`/`extern "C++" { ... }`
// block) is deliberately SKIPPED here, never pushed onto the chain: it is
// not a named C++ scope at all (it contributes nothing to a qualified
// name -- `extern "C" { void f(); }` names `::f`, not `::<something>::f`),
// so including it was always semantically wrong, not just cosmetic. It was
// also a real, corpus-observed source of order-dependence: unlike an
// ordinary namespace/record ancestor, whether libclang's semantic-parent
// chain reports a LinkageSpec ancestor for a given system typedef (e.g.
// glibc's `int32_t`/`int64_t`, CUDA's `CUuuid_st`) is NOT stable across
// separately-parsed translation units -- each TU's own macro/header state
// can genuinely nest (or not nest) the same logical declaration inside an
// `extern "C"` block differently, and `SrctrldbBackend::ensureSymbolNode()`
// only ever calls buildName() ONCE per SymbolKey per session (the first TU
// to observe a given USR wins; later observations just dedup via m_byKey)
// -- so which TU happened to be first decided whether the resulting
// NameHierarchy carried a spurious extra empty-name element, differing
// between e.g. a full `symhow index` and a `symhow update` resume that reindexes a
// different TU subset/order. Skipping LinkageSpec ancestors entirely
// removes the instability at its source (there's no "sometimes present,
// sometimes not" element to disagree about) while also fixing the
// pre-existing cosmetic bug of a bogus empty NameElement showing up in
// qualified names of anything declared inside `extern "C"` at all.
std::vector<CXCursor> ancestorChainRootToLeaf(CXCursor cursor)
{
    std::vector<CXCursor> chain;
    CXCursor cur = clang_getCursorSemanticParent(cursor);
    while (!clang_Cursor_isNull(cur) && clang_getCursorKind(cur) != CXCursor_TranslationUnit)
    {
        const CXCursor canonicalCur = clang_getCanonicalCursor(cur);
        if (clang_getCursorKind(canonicalCur) != CXCursor_LinkageSpec)
        {
            chain.push_back(canonicalCur);
        }
        // Always advance from the CANONICAL cursor (whether or not it was
        // pushed) -- see the comment above for why identity/naming must be
        // computed from the same canonical cursors throughout.
        cur = clang_getCursorSemanticParent(canonicalCur);
    }
    std::reverse(chain.begin(), chain.end());
    return chain;
}

// Whether `canonical`'s OWN semantic-parent chain contains an anonymous
// namespace (i.e. it is physically declared inside one) -- a purely
// structural check, deliberately NOT based on clang_getCursorLinkage():
// linkage information for an anonymous-namespace member can read back as
// either CXLinkage_Internal or CXLinkage_UniqueExternal depending on the
// query context (observed in practice: the SAME entity reported
// differently when visited directly vs. reached via
// clang_getCursorReferenced() from inside an unrelated template
// instantiation), which would make the disambiguation decision
// nondeterministic. It also, correctly, does NOT fire for an entity that
// merely *references* an anonymous-namespace type in its own signature
// (e.g. a normal namespace-scope function returning
// std::vector<AnonNsType>) -- only for entities actually nested inside one.
bool hasAnonymousNamespaceAncestor(CXCursor canonical)
{
    for (CXCursor cur = clang_getCursorSemanticParent(canonical);
         !clang_Cursor_isNull(cur) && clang_getCursorKind(cur) != CXCursor_TranslationUnit;
         cur = clang_getCursorSemanticParent(cur))
    {
        if (clang_getCursorKind(cur) == CXCursor_Namespace && toString(clang_getCursorSpelling(cur)).empty())
        {
            return true;
        }
    }
    return false;
}

// identityScope(cursor) == TranslationUnitLocal, per the SymbolKey design
// (see NameBuilder.h). `canonical` must already be clang_getCanonicalCursor()'d.
bool isLocalScope(CXCursor canonical)
{
    const CXLinkageKind linkage = clang_getCursorLinkage(canonical);
    const std::string spelling = toString(clang_getCursorSpelling(canonical));
    const CXCursor parent = clang_getCursorSemanticParent(canonical);
    const bool parentIsTu = !clang_Cursor_isNull(parent) && clang_getCursorKind(parent) == CXCursor_TranslationUnit;

    // CXLinkage_Internal covers `static` functions/variables at namespace
    // scope; hasAnonymousNamespaceAncestor() covers entities physically
    // declared inside an anonymous namespace (see its own doc comment for
    // why that's a structural check rather than a linkage check).
    return (linkage == CXLinkage_Internal) || hasAnonymousNamespaceAncestor(canonical) || (spelling == "main" && parentIsTu);
}

// Hidden disambiguator appended to a translation-unit-local entity's own
// NameElement postfix (see NameBuilder.h). `canonical` must already be
// clang_getCanonicalCursor()'d.
std::string disambiguatorFor(CXCursor canonical)
{
    CXSourceLocation loc = clang_getCursorLocation(canonical);
    CXFile file = nullptr;
    unsigned line = 0, col = 0, offset = 0;
    clang_getFileLocation(loc, &file, &line, &col, &offset);
    const std::string declFile = normalizedFileName(file);
    return " [" + basenameOf(declFile) + "#" + hex8(fnv1a64(declFile)) + "]";
}

// clang's own type/cursor spelling for a closure (lambda) type embeds a
// SYNTHESIZED display name of the form "(lambda at <path>:<line>:<col>)" --
// e.g. inside a `std::thread`/`std::function`-like template argument's
// printed type, or an anonymous-namespace-like display name for the
// closure type itself. `<path>` there comes straight from clang's own
// PrintingPolicy/SourceManager, NOT from clangutil::fileName()/
// normalizedFileName() (those only ever see file paths WE explicitly ask
// libclang for via clang_getFileLocation()), so the same instability
// normalizedFileName() fixes for ordinary declFile-based identity/
// disambiguator strings above reappears here through a completely
// different code path: the exact same lambda, reached via two TUs that
// #include its enclosing header with different relative spellings, prints
// with two different embedded paths (real, corpus-observed: cucs's
// `io_worker.hpp`, included from `test/` both via a canonical relative
// path and via a `test/data/../../src/data/...` one) -- which, because a
// function parameter's printed TYPE (e.g. `std::thread::thread(F&&)`'s `F`)
// becomes part of a NameElement's postfix, made the surrounding function's
// own qualified name (and therefore its node identity) TU-order-dependent
// too. Normalizing every embedded "(lambda at ...)" path found anywhere in
// prefix/name/postfix text closes this the same way normalizedFileName()
// does for the other cases.
std::string normalizeLambdaPathsInText(const std::string& text)
{
    static const std::regex kLambdaAt(R"(\(lambda at ([^():]+):(\d+):(\d+)\))");
    std::string result;
    result.reserve(text.size());
    std::size_t lastPos = 0;
    for (auto it = std::sregex_iterator(text.begin(), text.end(), kLambdaAt); it != std::sregex_iterator(); ++it)
    {
        const std::smatch& m = *it;
        const auto matchStart = static_cast<std::size_t>(m.position(0));
        result.append(text, lastPos, matchStart - lastPos);
        result += "(lambda at " + symhow::common::paths::normalize(m[1].str()) + ":" + m[2].str() + ":" + m[3].str() + ")";
        lastPos = matchStart + static_cast<std::size_t>(m.length(0));
    }
    result.append(text, lastPos, std::string::npos);
    return result;
}

// elementForCursor() plus the SAME local-scope disambiguation
// buildSymbolIdentity() applies to a symbol's own identity -- applied here
// to EVERY element of a Name hierarchy (not just the final/leaf one).
// This matters because buildName() is used both to compute an entity's own
// identity (where it's the last element) AND to compute the ancestor
// elements of some OTHER, deeper entity's hierarchy (e.g. Widget as an
// ancestor of Widget::touch()) -- without applying the SAME disambiguation
// consistently in both roles, the exact same logical entity would
// serialize to two DIFFERENT NameHierarchy strings depending on whether it
// was being identified directly or merely walked through as a prefix,
// causing the vendored writer's own addNodeHierarchy() to silently create
// a second, orphaned (never-kinded) node for the "prefix" spelling.
NameElement identityElement(CXCursor canonical)
{
    NameElement el = elementForCursor(canonical);
    if (isLocalScope(canonical))
    {
        el.postfix += disambiguatorFor(canonical);
    }
    el.prefix = normalizeLambdaPathsInText(el.prefix);
    el.name = normalizeLambdaPathsInText(el.name);
    el.postfix = normalizeLambdaPathsInText(el.postfix);
    return el;
}

}  // namespace

Name buildName(CXCursor cursor)
{
    const CXCursor canonical = clang_getCanonicalCursor(cursor);

    Name name;
    name.delimiter = "::";
    for (CXCursor ancestor : ancestorChainRootToLeaf(canonical))
    {
        name.elements.push_back(identityElement(ancestor));
    }
    name.elements.push_back(identityElement(canonical));
    return name;
}

SymbolIdentity buildSymbolIdentity(CXCursor cursor)
{
    const CXCursor canonical = clang_getCanonicalCursor(cursor);

    SymbolIdentity identity;
    identity.name = buildName(canonical);

    std::string usr = toString(clang_getCursorUSR(canonical));
    if (usr.empty())
    {
        // Some cursor kinds have no real USR at all -- most commonly
        // CXCursor_LinkageSpec (`extern "C" { ... }` blocks), which show up
        // constantly as ancestors of libc/libstdc++ declarations. Without
        // this fallback, every such cursor would collapse onto the SAME
        // (empty-string) SymbolKey, so registering one nested extern "C"
        // block as an ancestor of some decl would silently short-circuit
        // (as a "dedup hit") instead of actually calling recordSymbol() for
        // it -- leaving the node the vendored writer implicitly creates
        // for it (as a side effect of some OTHER, unrelated decl's longer
        // hierarchy) forever untyped (NodeKind::UNKNOWN), since our own
        // ancestor-registration loop never separately visited it. Falling
        // back to (cursor kind, canonical decl file, byte offset) keeps
        // identity deterministic and still dedups correctly across TUs
        // that include the exact same header (same file + offset), while
        // never colliding two textually-different no-USR entities.
        CXSourceLocation noUsrLoc = clang_getCursorLocation(canonical);
        CXFile noUsrFile = nullptr;
        unsigned noUsrLine = 0, noUsrCol = 0, noUsrOffset = 0;
        clang_getFileLocation(noUsrLoc, &noUsrFile, &noUsrLine, &noUsrCol, &noUsrOffset);
        usr = "no-usr:" + std::to_string(static_cast<int>(clang_getCursorKind(canonical))) + ":" +
              normalizedFileName(noUsrFile) + ":" + std::to_string(noUsrOffset);
    }

    identity.key = usr;
    if (isLocalScope(canonical))
    {
        CXSourceLocation loc = clang_getCursorLocation(canonical);
        CXFile file = nullptr;
        unsigned line = 0, col = 0, offset = 0;
        clang_getFileLocation(loc, &file, &line, &col, &offset);
        identity.key += '\x1f';
        identity.key += normalizedFileName(file);
    }

    return identity;
}

std::string buildLocalSymbolName(CXCursor cursor)
{
    const CXCursor canonical = clang_getCanonicalCursor(cursor);
    CXSourceLocation loc = clang_getCursorLocation(canonical);
    CXFile file = nullptr;
    unsigned line = 0, col = 0, offset = 0;
    clang_getFileLocation(loc, &file, &line, &col, &offset);
    const std::string path = file != nullptr ? normalizedFileName(file) : std::string("<unknown>");
    return path + ":" + std::to_string(line) + ":" + std::to_string(col);
}

}  // namespace symhow::index
