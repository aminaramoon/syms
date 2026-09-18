#pragma once

#include <unordered_set>
#include <vector>

#include <clang-c/Index.h>

#include "kg/index/FileRegistry.h"
#include "kg/model/FactSink.h"

// Walks the AST of one already-parsed translation unit via manual recursion
// through clang_visitChildren(), emitting kg::model facts into a FactSink.
// See docs on the individual row handlers in AstWalker.cpp for the mapping
// from libclang cursor kinds to facts (mirrors the AST Walker table in the
// Phase 3 design).
namespace kg::index
{

class AstWalker
{
public:
    AstWalker(CXTranslationUnit tu, FileRegistry& files, kg::model::FactSink& sink);

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
    kg::model::LocalId ensureSymbol(CXCursor cursor);
    kg::model::LocalId registerOne(CXCursor canonicalCursor);

    kg::model::LocalId fileFactId(CXFile file);
    kg::model::LocalId fileSymbolId(CXFile file);
    kg::model::LocalId currentContext();

    CXFile cursorFile(CXCursor c) const;
    bool rangeToModelRange(CXSourceRange range, kg::model::SourceRange& out);
    void emitOccurrenceForRange(kg::model::ElementClass cls, kg::model::LocalId element, CXSourceRange range, kg::model::LocationKind kind);

    CXSourceRange findCallLocation(CXCursor callExpr, CXCursor callee);
    void claimLocation(CXSourceRange range);
    bool isClaimedByCall(CXSourceRange range) const;

    CXTranslationUnit m_tu;
    FileRegistry& m_files;
    kg::model::FactSink& m_sink;

    std::vector<kg::model::LocalId> m_symbolStack;  // innermost-enclosing-symbol context stack
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

}  // namespace kg::index
