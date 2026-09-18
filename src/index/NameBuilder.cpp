#include "kg/index/NameBuilder.h"

#include <algorithm>
#include <filesystem>
#include <vector>

#include "kg/index/ClangUtil.h"

namespace kg::index
{

namespace
{

using kg::index::clangutil::fnv1a64;
using kg::index::clangutil::hex8;
using kg::index::clangutil::toString;
using kg::model::Name;
using kg::model::NameElement;

std::string basenameOf(const std::string& path)
{
    return std::filesystem::path(path).filename().string();
}

// file:line for an anonymous record/enum's synthesized display name.
void cursorFileAndLine(CXCursor c, std::string& file, unsigned& line)
{
    CXSourceLocation loc = clang_getCursorLocation(c);
    CXFile f = nullptr;
    unsigned col = 0, offset = 0;
    clang_getFileLocation(loc, &f, &line, &col, &offset);
    file = clangutil::fileName(f);
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
std::vector<CXCursor> ancestorChainRootToLeaf(CXCursor cursor)
{
    std::vector<CXCursor> chain;
    CXCursor cur = clang_getCursorSemanticParent(cursor);
    while (!clang_Cursor_isNull(cur) && clang_getCursorKind(cur) != CXCursor_TranslationUnit)
    {
        chain.push_back(clang_getCanonicalCursor(cur));
        cur = clang_getCursorSemanticParent(chain.back());
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
    const std::string declFile = clangutil::fileName(file);
    return " [" + basenameOf(declFile) + "#" + hex8(fnv1a64(declFile)) + "]";
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
              clangutil::fileName(noUsrFile) + ":" + std::to_string(noUsrOffset);
    }

    identity.key = usr;
    if (isLocalScope(canonical))
    {
        CXSourceLocation loc = clang_getCursorLocation(canonical);
        CXFile file = nullptr;
        unsigned line = 0, col = 0, offset = 0;
        clang_getFileLocation(loc, &file, &line, &col, &offset);
        identity.key += '\x1f';
        identity.key += clangutil::fileName(file);
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
    const std::string path = file != nullptr ? clangutil::fileName(file) : std::string("<unknown>");
    return path + ":" + std::to_string(line) + ":" + std::to_string(col);
}

}  // namespace kg::index
