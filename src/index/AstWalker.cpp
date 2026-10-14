#include "symhow/index/AstWalker.h"

#include "symhow/index/ClangUtil.h"
#include "symhow/index/NameBuilder.h"

namespace symhow::index
{

using symhow::model::DefinitionKind;
using symhow::model::ElementClass;
using symhow::model::LocalId;
using symhow::model::LocationKind;
using symhow::model::ReferenceKind;
using symhow::model::SymbolKind;
using symhow::model::kNoLocal;

namespace
{

bool isRecordLikeKind(CXCursorKind k)
{
    return k == CXCursor_StructDecl || k == CXCursor_ClassDecl || k == CXCursor_UnionDecl ||
           k == CXCursor_ClassTemplate || k == CXCursor_ClassTemplatePartialSpecialization;
}

enum class VarDeclKind
{
    Global,
    Field,
    Local,
};

VarDeclKind classifyVarDecl(CXCursor c)
{
    CXCursor parent = clang_getCursorSemanticParent(c);
    if (clang_Cursor_isNull(parent))
    {
        return VarDeclKind::Local;
    }
    const CXCursorKind pk = clang_getCursorKind(parent);
    if (pk == CXCursor_Namespace || pk == CXCursor_TranslationUnit)
    {
        return VarDeclKind::Global;
    }
    if (isRecordLikeKind(pk) && clang_Cursor_getStorageClass(c) == CX_SC_Static)
    {
        return VarDeclKind::Field;
    }
    return VarDeclKind::Local;
}

bool isLocalDeclKind(CXCursor c)
{
    const CXCursorKind k = clang_getCursorKind(c);
    if (k == CXCursor_ParmDecl || k == CXCursor_TemplateTypeParameter || k == CXCursor_NonTypeTemplateParameter ||
        k == CXCursor_TemplateTemplateParameter)
    {
        return true;
    }
    if (k == CXCursor_VarDecl)
    {
        return classifyVarDecl(c) == VarDeclKind::Local;
    }
    return false;
}

SymbolKind classifyKind(CXCursor c)
{
    switch (clang_getCursorKind(c))
    {
        case CXCursor_Namespace:
            return SymbolKind::Namespace;
        case CXCursor_StructDecl:
            return SymbolKind::Struct;
        case CXCursor_ClassDecl:
        case CXCursor_ClassTemplate:
        case CXCursor_ClassTemplatePartialSpecialization:
            return SymbolKind::Class;
        case CXCursor_UnionDecl:
            return SymbolKind::Union;
        case CXCursor_EnumDecl:
            return SymbolKind::Enum;
        case CXCursor_EnumConstantDecl:
            return SymbolKind::EnumConstant;
        case CXCursor_FieldDecl:
            return SymbolKind::Field;
        case CXCursor_VarDecl:
        {
            const VarDeclKind vk = classifyVarDecl(c);
            if (vk == VarDeclKind::Field)
            {
                return SymbolKind::Field;
            }
            // Local vars never reach classifyKind() via ensureSymbol() (see
            // isLocalDeclKind() gate in the dispatcher); if one does slip
            // through via an odd ancestor-chain edge case, Type is a safe,
            // never-UNKNOWN fallback.
            return vk == VarDeclKind::Global ? SymbolKind::GlobalVariable : SymbolKind::Type;
        }
        case CXCursor_FunctionDecl:
        case CXCursor_FunctionTemplate:
            return SymbolKind::Function;
        case CXCursor_CXXMethod:
        case CXCursor_Constructor:
        case CXCursor_Destructor:
        case CXCursor_ConversionFunction:
            return SymbolKind::Method;
        case CXCursor_TypedefDecl:
        case CXCursor_TypeAliasDecl:
        case CXCursor_TypeAliasTemplateDecl:
            return SymbolKind::Typedef;
        case CXCursor_MacroDefinition:
            return SymbolKind::Macro;
        default:
            return SymbolKind::Type;  // fallback per spec
    }
}

struct FindReferencingChild
{
    CXCursor target;
    CXCursor found = clang_getNullCursor();
    bool hasFound = false;
};

CXChildVisitResult findReferencingChildVisitor(CXCursor c, CXCursor /*parent*/, CXClientData data)
{
    auto* d = static_cast<FindReferencingChild*>(data);
    const CXCursorKind k = clang_getCursorKind(c);
    if (k == CXCursor_DeclRefExpr || k == CXCursor_MemberRefExpr)
    {
        CXCursor ref = clang_getCursorReferenced(c);
        if (!clang_Cursor_isNull(ref) && clang_equalCursors(ref, d->target) != 0)
        {
            d->found = c;
            d->hasFound = true;
            return CXChildVisit_Break;
        }
    }
    return CXChildVisit_Continue;  // one level only -- never recurse into grandchildren
}

CXChildVisitResult findChildOfKindVisitor(CXCursor c, CXCursor /*parent*/, CXClientData data)
{
    auto* out = static_cast<CXCursor*>(data);
    if (clang_getCursorKind(c) == CXCursor_Destructor)
    {
        *out = c;
        return CXChildVisit_Break;
    }
    return CXChildVisit_Continue;
}

}  // namespace

struct AstWalker::ContextFrame
{
    AstWalker& walker;
    bool pushed;

    ContextFrame(AstWalker& w, LocalId sym) : walker(w), pushed(sym != kNoLocal)
    {
        if (pushed)
        {
            walker.m_symbolStack.push_back(sym);
        }
    }
    ~ContextFrame()
    {
        if (pushed)
        {
            walker.m_symbolStack.pop_back();
        }
    }
    ContextFrame(const ContextFrame&) = delete;
    ContextFrame& operator=(const ContextFrame&) = delete;
};

AstWalker::AstWalker(CXTranslationUnit tu, FileRegistry& files, symhow::model::FactSink& sink)
    : m_tu(tu), m_files(files), m_sink(sink)
{
}

CXChildVisitResult AstWalker::trampoline(CXCursor cursor, CXCursor parent, CXClientData clientData)
{
    return static_cast<AstWalker*>(clientData)->visit(cursor, parent);
}

void AstWalker::inclusionTrampoline(CXFile includedFile, CXSourceLocation* stack, unsigned includeLen, CXClientData clientData)
{
    static_cast<AstWalker*>(clientData)->handleInclusion(includedFile, stack, includeLen);
}

void AstWalker::run()
{
    processInclusions();
    CXCursor root = clang_getTranslationUnitCursor(m_tu);
    clang_visitChildren(root, &AstWalker::trampoline, this);
}

// --- File rows + INCLUDE edges ---------------------------------------------

void AstWalker::processInclusions()
{
    clang_getInclusions(m_tu, &AstWalker::inclusionTrampoline, this);
}

void AstWalker::handleInclusion(CXFile includedFile, CXSourceLocation* stack, unsigned includeLen)
{
    CXFile includerFile = nullptr;
    if (includeLen > 0)
    {
        CXFile f = nullptr;
        unsigned l = 0, col = 0, off = 0;
        clang_getFileLocation(stack[0], &f, &l, &col, &off);
        includerFile = f;
    }

    bool includerIndexed = false;
    if (includerFile != nullptr)
    {
        includerIndexed = m_files.infoFor(m_tu, includerFile).indexed;
        if (includerIndexed)
        {
            m_files.markDirectIncludeOfIndexed(m_tu, includedFile);
        }
    }

    if (!m_files.shouldRegister(m_tu, includedFile))
    {
        return;
    }
    fileFactId(includedFile);

    if (includerFile != nullptr && includerIndexed)
    {
        const LocalId includerSym = fileSymbolId(includerFile);
        const LocalId includedSym = fileSymbolId(includedFile);
        const LocalId refId = m_sink.reference(includerSym, includedSym, ReferenceKind::Include);

        CXFile locFile = nullptr;
        unsigned line = 0, col = 0, off = 0;
        clang_getFileLocation(stack[0], &locFile, &line, &col, &off);
        if (locFile != nullptr)
        {
            symhow::model::SourceRange range;
            range.file = fileFactId(locFile);
            range.startLine = line;
            range.startCol = col;
            range.endLine = line;
            range.endCol = col;
            m_sink.occurrence(ElementClass::Reference, refId, range, LocationKind::Token);
        }
    }
}

// --- Cursor dispatch ---------------------------------------------------

CXFile AstWalker::cursorFile(CXCursor c) const
{
    CXSourceLocation loc = clang_getCursorLocation(c);
    CXFile file = nullptr;
    unsigned line = 0, col = 0, off = 0;
    clang_getFileLocation(loc, &file, &line, &col, &off);
    return file;
}

CXChildVisitResult AstWalker::visit(CXCursor c, CXCursor /*parent*/)
{
    CXFile file = cursorFile(c);
    if (file == nullptr)
    {
        return CXChildVisit_Continue;
    }
    if (!m_files.infoFor(m_tu, file).indexed)
    {
        return CXChildVisit_Continue;
    }
    m_currentFile = file;

    switch (clang_getCursorKind(c))
    {
        case CXCursor_Namespace:
        case CXCursor_ClassDecl:
        case CXCursor_StructDecl:
        case CXCursor_UnionDecl:
        case CXCursor_EnumDecl:
        case CXCursor_ClassTemplate:
        case CXCursor_ClassTemplatePartialSpecialization:
            return visitScopeDecl(c);

        case CXCursor_FunctionDecl:
        case CXCursor_FunctionTemplate:
        case CXCursor_CXXMethod:
        case CXCursor_Constructor:
        case CXCursor_Destructor:
        case CXCursor_ConversionFunction:
            return visitFunctionLikeDecl(c);

        case CXCursor_TypedefDecl:
        case CXCursor_TypeAliasDecl:
        case CXCursor_TypeAliasTemplateDecl:
        case CXCursor_EnumConstantDecl:
        case CXCursor_FieldDecl:
            handleSimpleDecl(c);
            return CXChildVisit_Recurse;

        case CXCursor_VarDecl:
            handleVarDecl(c);
            return CXChildVisit_Recurse;

        case CXCursor_ParmDecl:
        case CXCursor_TemplateTypeParameter:
        case CXCursor_NonTypeTemplateParameter:
        case CXCursor_TemplateTemplateParameter:
            handleLocalDecl(c);
            return CXChildVisit_Recurse;

        case CXCursor_MacroDefinition:
            handleMacroDefinition(c);
            return CXChildVisit_Continue;

        case CXCursor_MacroExpansion:
            handleMacroExpansion(c);
            return CXChildVisit_Continue;

        case CXCursor_CallExpr:
            handleCallExpr(c);
            return CXChildVisit_Recurse;

        case CXCursor_CXXDeleteExpr:
            handleDeleteExpr(c);
            return CXChildVisit_Recurse;

        case CXCursor_DeclRefExpr:
        case CXCursor_MemberRefExpr:
            handleDeclOrMemberRef(c);
            return CXChildVisit_Recurse;

        case CXCursor_TypeRef:
        case CXCursor_TemplateRef:
            handleTypeOrTemplateRef(c);
            return CXChildVisit_Recurse;

        case CXCursor_NamespaceRef:
            handleNamespaceRef(c);
            return CXChildVisit_Recurse;

        case CXCursor_CXXBaseSpecifier:
            handleBaseSpecifier(c);
            // Deliberately do not recurse: the base-specifier's own TypeRef
            // is already fully captured by the INHERITANCE edge, and
            // skipping it keeps TYPE_USAGE free of base-specifier noise
            // (see AstWalker design notes on CXXBaseSpecifier).
            return CXChildVisit_Continue;

        default:
            return CXChildVisit_Recurse;
    }
}

// --- Scope-introducing decls (namespace/class/struct/union/enum) -----------

CXChildVisitResult AstWalker::visitScopeDecl(CXCursor c)
{
    const LocalId id = ensureSymbol(c);
    markDeclaredHere(id);

    emitOccurrenceForRange(ElementClass::Symbol, id, clang_Cursor_getSpellingNameRange(c, 0, 0), LocationKind::Token);
    if (clang_isCursorDefinition(c) != 0)
    {
        emitOccurrenceForRange(ElementClass::Symbol, id, clang_getCursorExtent(c), LocationKind::Scope);
    }

    {
        ContextFrame frame(*this, id);
        clang_visitChildren(c, &AstWalker::trampoline, this);
    }
    return CXChildVisit_Continue;
}

CXChildVisitResult AstWalker::visitFunctionLikeDecl(CXCursor c)
{
    const LocalId id = ensureSymbol(c);
    markDeclaredHere(id);

    emitOccurrenceForRange(ElementClass::Symbol, id, clang_Cursor_getSpellingNameRange(c, 0, 0), LocationKind::Token);
    if (clang_isCursorDefinition(c) != 0)
    {
        emitOccurrenceForRange(ElementClass::Symbol, id, clang_getCursorExtent(c), LocationKind::Scope);
    }
    else
    {
        emitOccurrenceForRange(ElementClass::Symbol, id, clang_getCursorExtent(c), LocationKind::Signature);
    }

    const CXCursorKind kind = clang_getCursorKind(c);
    if (kind == CXCursor_CXXMethod || kind == CXCursor_Destructor)
    {
        CXCursor* overridden = nullptr;
        unsigned count = 0;
        clang_getOverriddenCursors(c, &overridden, &count);
        for (unsigned i = 0; i < count; ++i)
        {
            if (!rangeFileIndexed(spellingRangeOrPoint(c)))
            {
                continue;
            }
            const LocalId targetId = ensureSymbol(overridden[i]);
            const LocalId refId = m_sink.reference(id, targetId, ReferenceKind::Override);
            emitOccurrenceForRange(ElementClass::Reference, refId, spellingRangeOrPoint(c), LocationKind::Token);
        }
        if (overridden != nullptr)
        {
            clang_disposeOverriddenCursors(overridden);
        }
    }

    {
        ContextFrame frame(*this, id);
        clang_visitChildren(c, &AstWalker::trampoline, this);
    }
    return CXChildVisit_Continue;
}

// --- Simple (non-scope-introducing) decls -----------------------------

void AstWalker::handleSimpleDecl(CXCursor c)
{
    const LocalId id = ensureSymbol(c);
    markDeclaredHere(id);
    emitOccurrenceForRange(ElementClass::Symbol, id, clang_Cursor_getSpellingNameRange(c, 0, 0), LocationKind::Token);
}

void AstWalker::handleVarDecl(CXCursor c)
{
    if (classifyVarDecl(c) == VarDeclKind::Local)
    {
        handleLocalDecl(c);
        return;
    }
    handleSimpleDecl(c);
}

void AstWalker::handleLocalDecl(CXCursor c)
{
    const std::string name = buildLocalSymbolName(c);
    const LocalId id = m_sink.localSymbol(name);
    emitOccurrenceForRange(ElementClass::LocalSymbol, id, clang_Cursor_getSpellingNameRange(c, 0, 0), LocationKind::LocalSymbol);
}

// --- Macros --------------------------------------------------------------

void AstWalker::handleMacroDefinition(CXCursor c)
{
    if (clang_Cursor_isMacroBuiltin(c) != 0)
    {
        return;
    }
    const LocalId id = ensureSymbol(c);
    markDeclaredHere(id);
    emitOccurrenceForRange(ElementClass::Symbol, id, clang_getCursorExtent(c), LocationKind::Token);
}

void AstWalker::handleMacroExpansion(CXCursor c)
{
    CXCursor referenced = clang_getCursorReferenced(c);
    if (clang_Cursor_isNull(referenced) || clang_Cursor_isMacroBuiltin(referenced) != 0)
    {
        return;
    }

    CXFile file = cursorFile(c);
    if (file == nullptr)
    {
        return;
    }

    const CXSourceRange range = clang_getCursorExtent(c);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }

    const LocalId ctx = fileSymbolId(file);
    const LocalId targetId = ensureSymbol(referenced);
    const LocalId refId = m_sink.reference(ctx, targetId, ReferenceKind::MacroUsage);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

// --- Calls -----------------------------------------------------------------

CXSourceRange AstWalker::findCallLocation(CXCursor callExpr, CXCursor callee)
{
    if (!clang_Cursor_isNull(callee))
    {
        FindReferencingChild data;
        data.target = callee;
        clang_visitChildren(callExpr, &findReferencingChildVisitor, &data);
        if (data.hasFound)
        {
            CXSourceRange r = clang_Cursor_getSpellingNameRange(data.found, 0, 0);
            if (clang_Range_isNull(r) == 0)
            {
                return r;
            }
        }
    }

    CXSourceRange self = clang_Cursor_getSpellingNameRange(callExpr, 0, 0);
    if (clang_Range_isNull(self) == 0)
    {
        return self;
    }

    CXSourceLocation loc = clang_getCursorLocation(callExpr);
    return clang_getRange(loc, loc);
}

void AstWalker::claimLocation(CXSourceRange range)
{
    CXSourceLocation loc = clang_getRangeStart(range);
    CXFile f = nullptr;
    unsigned l = 0, c = 0, off = 0;
    clang_getFileLocation(loc, &f, &l, &c, &off);
    m_claimedLocations.insert(LocKey{f, off});
}

bool AstWalker::isClaimedByCall(CXSourceRange range) const
{
    CXSourceLocation loc = clang_getRangeStart(range);
    CXFile f = nullptr;
    unsigned l = 0, c = 0, off = 0;
    clang_getFileLocation(loc, &f, &l, &c, &off);
    return m_claimedLocations.count(LocKey{f, off}) > 0;
}

void AstWalker::handleCallExpr(CXCursor c)
{
    CXCursor callee = clang_getCursorReferenced(c);
    CXSourceRange range = findCallLocation(c, callee);
    claimLocation(range);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }

    const LocalId ctx = currentContext();
    if (clang_Cursor_isNull(callee))
    {
        const LocalId refId = m_sink.unsolvedReference(ctx, ReferenceKind::Call);
        emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
        return;
    }

    const LocalId targetId = ensureSymbol(callee);
    const LocalId refId = m_sink.reference(ctx, targetId, ReferenceKind::Call);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

void AstWalker::handleDeleteExpr(CXCursor c)
{
    CXCursor operand = clang_getNullCursor();
    struct FirstChild
    {
        CXCursor found = clang_getNullCursor();
        bool has = false;
    } first;
    clang_visitChildren(
        c,
        [](CXCursor child, CXCursor, CXClientData data) -> CXChildVisitResult {
            auto* out = static_cast<FirstChild*>(data);
            out->found = child;
            out->has = true;
            return CXChildVisit_Break;
        },
        &first);
    if (!first.has)
    {
        return;
    }
    operand = first.found;

    const CXType opType = clang_getCursorType(operand);
    const CXType pointee = clang_getPointeeType(opType);
    CXCursor recordDecl = clang_getTypeDeclaration(pointee);
    if (clang_Cursor_isNull(recordDecl))
    {
        return;
    }

    CXCursor dtor = clang_getNullCursor();
    clang_visitChildren(recordDecl, &findChildOfKindVisitor, &dtor);
    if (clang_Cursor_isNull(dtor))
    {
        // Compiler-implicit destructor: no cursor exists to point at.
        // Documented gap (see KNOWN_LIMITATIONS.md).
        return;
    }

    const CXSourceRange range = spellingRangeOrPoint(c);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }

    const LocalId targetId = ensureSymbol(dtor);
    const LocalId ctx = currentContext();
    const LocalId refId = m_sink.reference(ctx, targetId, ReferenceKind::Call);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

// --- Usages / type usages / qualifiers / inheritance ------------------

void AstWalker::handleDeclOrMemberRef(CXCursor c)
{
    CXSourceRange range = clang_getCursorReferenceNameRange(c, CXNameRange_WantSinglePiece, 0);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }
    if (isClaimedByCall(range))
    {
        return;
    }

    CXCursor referenced = clang_getCursorReferenced(c);
    const LocalId ctx = currentContext();

    if (clang_Cursor_isNull(referenced))
    {
        const LocalId refId = m_sink.unsolvedReference(ctx, ReferenceKind::Usage);
        emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
        return;
    }

    if (isLocalDeclKind(referenced))
    {
        const std::string name = buildLocalSymbolName(referenced);
        const LocalId id = m_sink.localSymbol(name);
        emitOccurrenceForRange(ElementClass::LocalSymbol, id, range, LocationKind::LocalSymbol);
        return;
    }

    const LocalId targetId = ensureSymbol(referenced);
    const LocalId refId = m_sink.reference(ctx, targetId, ReferenceKind::Usage);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

void AstWalker::handleTypeOrTemplateRef(CXCursor c)
{
    CXCursor referenced = clang_getCursorReferenced(c);
    const LocalId ctx = currentContext();
    CXSourceRange range = spellingRangeOrPoint(c);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }

    if (clang_Cursor_isNull(referenced))
    {
        const LocalId refId = m_sink.unsolvedReference(ctx, ReferenceKind::TypeUsage);
        emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
        return;
    }

    const LocalId targetId = ensureSymbol(referenced);
    const LocalId refId = m_sink.reference(ctx, targetId, ReferenceKind::TypeUsage);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

void AstWalker::handleNamespaceRef(CXCursor c)
{
    CXCursor referenced = clang_getCursorReferenced(c);
    if (clang_Cursor_isNull(referenced))
    {
        return;
    }
    if (!rangeFileIndexed(clang_getCursorExtent(c)))
    {
        return;
    }
    const LocalId targetId = ensureSymbol(referenced);
    emitOccurrenceForRange(ElementClass::Symbol, targetId, clang_getCursorExtent(c), LocationKind::Qualifier);
}

void AstWalker::handleBaseSpecifier(CXCursor c)
{
    if (m_symbolStack.empty())
    {
        return;
    }
    const LocalId derivedId = m_symbolStack.back();

    CXCursor base = clang_getCursorReferenced(c);
    if (clang_Cursor_isNull(base))
    {
        base = clang_getTypeDeclaration(clang_getCursorType(c));
    }
    if (clang_Cursor_isNull(base))
    {
        return;
    }

    const CXSourceRange range = spellingRangeOrPoint(c);
    // Fact gate (see rangeFileIndexed()): never create an edge we would not
    // be able to anchor with an occurrence in an indexed file.
    if (!rangeFileIndexed(range))
    {
        return;
    }

    const LocalId targetId = ensureSymbol(base);
    const LocalId refId = m_sink.reference(derivedId, targetId, ReferenceKind::Inheritance);
    emitOccurrenceForRange(ElementClass::Reference, refId, range, LocationKind::Token);
}

// --- Symbol registration ------------------------------------------------

LocalId AstWalker::ensureSymbol(CXCursor cursor)
{
    CXCursor canonical = clang_getCanonicalCursor(cursor);

    std::vector<CXCursor> chain;
    for (CXCursor cur = canonical; !clang_Cursor_isNull(cur) && clang_getCursorKind(cur) != CXCursor_TranslationUnit;)
    {
        // CXCursor_LinkageSpec (`extern "C"`/`extern "C++"` blocks) is
        // skipped here, mirroring NameBuilder::ancestorChainRootToLeaf's own
        // skip (see its comment): it isn't a named C++ scope, so it should
        // never become a symbol/node of its own, and -- the real bug this
        // fixes -- whether a given libc/CUDA typedef's semantic-parent walk
        // even PASSES THROUGH a LinkageSpec ancestor is not stable across
        // separately-parsed TUs (same logical declaration, different
        // macro/header state), which used to make it order-dependent
        // whether that TU minted a spurious extra "extern C block" symbol
        // node at all. Still walks past it to whatever lexically/
        // semantically encloses it, exactly like the NameBuilder skip does.
        if (clang_getCursorKind(cur) != CXCursor_LinkageSpec)
        {
            chain.push_back(cur);
        }
        CXCursor parent = clang_getCursorSemanticParent(cur);
        cur = clang_Cursor_isNull(parent) ? parent : clang_getCanonicalCursor(parent);
    }

    LocalId leafId = kNoLocal;
    for (auto it = chain.rbegin(); it != chain.rend(); ++it)
    {
        leafId = registerOne(*it);
    }
    return leafId;
}

LocalId AstWalker::registerOne(CXCursor canonicalCursor)
{
    SymbolIdentity identity = buildSymbolIdentity(canonicalCursor);

    symhow::model::SymbolFact fact;
    fact.key = std::move(identity.key);
    fact.name = std::move(identity.name);
    fact.kind = classifyKind(canonicalCursor);
    fact.definition = clang_isCursorDefinition(canonicalCursor) != 0 ? DefinitionKind::Explicit : DefinitionKind::None;
    return m_sink.symbol(std::move(fact));
}

void AstWalker::markDeclaredHere(LocalId id)
{
    if (id == kNoLocal)
    {
        return;
    }
    m_sink.refineSymbol(id, SymbolKind::Unknown, DefinitionKind::Explicit);
    if (m_currentFile != nullptr)
    {
        m_sink.declaredIn(id, fileFactId(m_currentFile));
    }
}

CXSourceRange AstWalker::spellingRangeOrPoint(CXCursor c)
{
    CXSourceRange range = clang_Cursor_getSpellingNameRange(c, 0, 0);
    if (clang_Range_isNull(range) != 0)
    {
        CXSourceLocation loc = clang_getCursorLocation(c);
        range = clang_getRange(loc, loc);
    }
    return range;
}

LocalId AstWalker::fileFactId(CXFile file)
{
    const FileRegistry::Info& info = m_files.infoFor(m_tu, file);
    symhow::model::FileFact fact;
    fact.path = info.path;
    fact.language = "cpp";
    fact.indexed = info.indexed;
    fact.system = info.system;
    fact.contentHash = info.contentHash;
    return m_sink.file(std::move(fact));
}

LocalId AstWalker::fileSymbolId(CXFile file)
{
    fileFactId(file);  // ensure the FileFact row exists too
    const FileRegistry::Info& info = m_files.infoFor(m_tu, file);

    symhow::model::SymbolFact fact;
    fact.key = info.path;  // SrctrldbBackend special-cases SymbolKind::File: key IS the absolute path
    fact.name.delimiter = "/";
    fact.name.elements.push_back(symhow::model::NameElement{"", info.path, ""});
    fact.kind = SymbolKind::File;
    fact.definition = DefinitionKind::None;
    return m_sink.symbol(std::move(fact));
}

LocalId AstWalker::currentContext()
{
    if (!m_symbolStack.empty())
    {
        return m_symbolStack.back();
    }
    if (m_currentFile != nullptr)
    {
        return fileSymbolId(m_currentFile);
    }
    return kNoLocal;
}

bool AstWalker::rangeFileIndexed(CXSourceRange range)
{
    if (clang_Range_isNull(range) != 0)
    {
        return false;
    }
    CXFile file = nullptr;
    unsigned line = 0, col = 0, off = 0;
    clang_getFileLocation(clang_getRangeStart(range), &file, &line, &col, &off);
    if (file == nullptr)
    {
        return false;
    }
    return m_files.infoFor(m_tu, file).indexed;
}

bool AstWalker::rangeToModelRange(CXSourceRange range, symhow::model::SourceRange& out)
{
    if (clang_Range_isNull(range) != 0)
    {
        return false;
    }
    CXSourceLocation startLoc = clang_getRangeStart(range);
    CXSourceLocation endLoc = clang_getRangeEnd(range);

    CXFile startFile = nullptr;
    unsigned sl = 0, sc = 0, so = 0;
    clang_getFileLocation(startLoc, &startFile, &sl, &sc, &so);
    if (startFile == nullptr)
    {
        return false;
    }

    CXFile endFile = nullptr;
    unsigned el = 0, ec = 0, eo = 0;
    clang_getFileLocation(endLoc, &endFile, &el, &ec, &eo);

    out.file = fileFactId(startFile);
    out.startLine = sl;
    out.startCol = sc;
    out.endLine = (endFile != nullptr) ? el : sl;
    out.endCol = (endFile != nullptr) ? ec : sc;
    return true;
}

void AstWalker::emitOccurrenceForRange(ElementClass cls, LocalId element, CXSourceRange range, LocationKind kind)
{
    if (element == kNoLocal)
    {
        return;
    }
    if (!rangeFileIndexed(range))
    {
        // Backstop for the per-handler gates below: an occurrence is only
        // ever emitted if ITS OWN range lands in an indexed file.
        return;
    }
    symhow::model::SourceRange modelRange;
    if (!rangeToModelRange(range, modelRange))
    {
        return;
    }
    m_sink.occurrence(cls, element, modelRange, kind);
}

}  // namespace symhow::index
