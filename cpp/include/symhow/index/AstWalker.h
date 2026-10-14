#pragma once

#include <unordered_set>
#include <vector>

#include <clang-c/Index.h>

#include "symhow/index/FileRegistry.h"
#include "symhow/model/FactSink.h"

// Walks the AST of one already-parsed translation unit via manual recursion
// through clang_visitChildren(), emitting symhow::model facts into a FactSink.
// See docs on the individual row handlers in AstWalker.cpp for the mapping
// from libclang cursor kinds to facts (mirrors the AST Walker table in the
// Phase 3 design).
namespace symhow::index
{

class AstWalker
{
public:
    AstWalker(CXTranslationUnit tu, FileRegistry& files, symhow::model::FactSink& sink);

    // Runs the whole walk: clang_getInclusions() (File rows + INCLUDE
    // edges) followed by a cursor-tree walk starting at the translation
    // unit cursor.
    void run();

private:
    struct ContextFrame;
    friend struct ContextFrame;

    static CXChildVisitResult trampoline(CXCursor cursor, CXCursor parent, CXClientData clientData);
    static void inclusionTrampoline(CXFile includedFile, CXSourceLocation* stack, unsigned includeLen, CXClientData clientData);

    CXChildVisitResult visit(CXCursor cursor, CXCursor parent);

    void processInclusions();
    void handleInclusion(CXFile includedFile, CXSourceLocation* stack, unsigned includeLen);

    // "Context introducing" decls: manually recurse (RAII-framed) so the
    // enclosing-symbol stack can be popped again after the subtree.
    CXChildVisitResult visitScopeDecl(CXCursor c);
    CXChildVisitResult visitFunctionLikeDecl(CXCursor c);

    void handleSimpleDecl(CXCursor c);   // EnumConstant/Field/Typedef-like/global VarDecl
    void handleVarDecl(CXCursor c);      // dispatch: local vs. field vs. global
    void handleLocalDecl(CXCursor c);    // ParmDecl / local VarDecl / template params
    void handleMacroDefinition(CXCursor c);
    void handleMacroExpansion(CXCursor c);
    void handleCallExpr(CXCursor c);
    void handleDeleteExpr(CXCursor c);
    void handleDeclOrMemberRef(CXCursor c);
    void handleTypeOrTemplateRef(CXCursor c);
    void handleNamespaceRef(CXCursor c);
    void handleBaseSpecifier(CXCursor c);

    // Symbol registration: always resolves via clang_getCanonicalCursor()
    // and always registers the FULL semantic-parent ancestor chain (each
    // ancestor as its own SymbolFact), not just the leaf -- otherwise
    // ancestor nodes implicitly created by the backend's addNodeHierarchy
    // would never get a real SymbolKind and stay NodeKind::UNKNOWN forever.
    // Used uniformly for both directly-visited decls and
    // clang_getCursorReferenced() fallback targets.
    symhow::model::LocalId ensureSymbol(CXCursor cursor);
    symhow::model::LocalId registerOne(CXCursor canonicalCursor);

    symhow::model::LocalId fileFactId(CXFile file);
    symhow::model::LocalId fileSymbolId(CXFile file);
    symhow::model::LocalId currentContext();

    // Marks `id` as Explicit-defined (declaration-in-indexed-file counts,
    // matching Sourcetrail's own semantics -- see docs/KNOWN_LIMITATIONS.md's
    // Phase 1 section) and records declaredIn(id, <m_currentFile's FileFact>).
    // Called from every "own declaration" site (visitScopeDecl,
    // visitFunctionLikeDecl, handleSimpleDecl, handleMacroDefinition).
    void markDeclaredHere(symhow::model::LocalId id);

    CXFile cursorFile(CXCursor c) const;

    // True iff `range` is non-null and the file its START location resolves
    // to is an INDEXED file. Every fact symhow emits (occurrence, and therefore
    // the reference edge that occurrence anchors) must pass this gate: a
    // cursor can live in an indexed .cpp yet have a spelling/reference range
    // that resolves INTO a non-indexed header (macro expansions resolve to
    // the macro's definition site; implicit instantiations resolve into the
    // template's own header). Recording those facts makes symhow describe files
    // it was explicitly told not to index -- and makes them unreachable for
    // `symhow update`, which can only ever clear facts located in files it
    // tracks. See docs/KNOWN_LIMITATIONS.md's Phase 5 section.
    bool rangeFileIndexed(CXSourceRange range);

    bool rangeToModelRange(CXSourceRange range, symhow::model::SourceRange& out);
    void emitOccurrenceForRange(symhow::model::ElementClass cls, symhow::model::LocalId element, CXSourceRange range, symhow::model::LocationKind kind);

    // A cursor's spelling-name range, falling back to a zero-width range at
    // the cursor's location if the spelling-name range is null (e.g. some
    // OVERRIDE/INHERITANCE targets, `delete expr`s, TypeRef/TemplateRef
    // cursors without a distinct spelling token). Guarantees every non-MEMBER
    // edge symhow emits ends up with >= 1 occurrence.
    static CXSourceRange spellingRangeOrPoint(CXCursor c);

    CXSourceRange findCallLocation(CXCursor callExpr, CXCursor callee);
    void claimLocation(CXSourceRange range);
    bool isClaimedByCall(CXSourceRange range) const;

    CXTranslationUnit m_tu;
    FileRegistry& m_files;
    symhow::model::FactSink& m_sink;

    std::vector<symhow::model::LocalId> m_symbolStack;  // innermost-enclosing-symbol context stack
    CXFile m_currentFile = nullptr;                 // file of the cursor currently being visited (context fallback)

    struct LocKey
    {
        const void* file;
        unsigned offset;
        bool operator==(const LocKey& o) const { return file == o.file && offset == o.offset; }
    };
    struct LocKeyHash
    {
        std::size_t operator()(const LocKey& k) const
        {
            return std::hash<const void*>{}(k.file) ^ (std::hash<unsigned>{}(k.offset) << 1);
        }
    };
    std::unordered_set<LocKey, LocKeyHash> m_claimedLocations;
};

}  // namespace symhow::index
