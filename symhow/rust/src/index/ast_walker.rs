//! Walks the AST of one translation unit, emitting model facts into a
//! FactSink. Mirrors `symhow::index::AstWalker`.

// clang-sys CamelCase constants trip non_upper_case_globals in patterns.
#![allow(non_upper_case_globals)]

use std::collections::HashSet;

use clang_sys::*;

use crate::common::content_hash::content_hash;
use crate::model::{
    DefinitionKind, ElementClass, FactSink, FileFact, LocalId, LocationKind, Name, NameElement,
    ReferenceKind, SourceRange, SymbolFact, SymbolKind, K_NO_LOCAL,
};

use super::clangutil::file_name;
use super::file_registry::{Info as FileInfo, FileKey, FileRegistry};
use super::name_builder::{build_local_symbol_name, build_symbol_identity};

fn is_record_like_kind(k: CXCursorKind) -> bool {
    k == CXCursor_StructDecl
        || k == CXCursor_ClassDecl
        || k == CXCursor_UnionDecl
        || k == CXCursor_ClassTemplate
        || k == CXCursor_ClassTemplatePartialSpecialization
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VarDeclKind {
    Global,
    Field,
    Local,
}

fn classify_var_decl(c: CXCursor) -> VarDeclKind {
    let parent = unsafe { clang_getCursorSemanticParent(c) };
    if unsafe { clang_Cursor_isNull(parent) } != 0 {
        return VarDeclKind::Local;
    }
    let pk = unsafe { clang_getCursorKind(parent) };
    if pk == CXCursor_Namespace || pk == CXCursor_TranslationUnit {
        return VarDeclKind::Global;
    }
    if is_record_like_kind(pk) && unsafe { clang_Cursor_getStorageClass(c) } == CX_SC_Static {
        return VarDeclKind::Field;
    }
    VarDeclKind::Local
}

fn is_local_decl_kind(c: CXCursor) -> bool {
    let k = unsafe { clang_getCursorKind(c) };
    if k == CXCursor_ParmDecl
        || k == CXCursor_TemplateTypeParameter
        || k == CXCursor_NonTypeTemplateParameter
        || k == CXCursor_TemplateTemplateParameter
    {
        return true;
    }
    if k == CXCursor_VarDecl {
        return classify_var_decl(c) == VarDeclKind::Local;
    }
    false
}

fn classify_kind(c: CXCursor) -> SymbolKind {
    match unsafe { clang_getCursorKind(c) } {
        CXCursor_Namespace => SymbolKind::Namespace,
        CXCursor_StructDecl => SymbolKind::Struct,
        CXCursor_ClassDecl | CXCursor_ClassTemplate | CXCursor_ClassTemplatePartialSpecialization => SymbolKind::Class,
        CXCursor_UnionDecl => SymbolKind::Union,
        CXCursor_EnumDecl => SymbolKind::Enum,
        CXCursor_EnumConstantDecl => SymbolKind::EnumConstant,
        CXCursor_FieldDecl => SymbolKind::Field,
        CXCursor_VarDecl => match classify_var_decl(c) {
            VarDeclKind::Field => SymbolKind::Field,
            VarDeclKind::Global => SymbolKind::GlobalVariable,
            VarDeclKind::Local => SymbolKind::Type,
        },
        CXCursor_FunctionDecl | CXCursor_FunctionTemplate => SymbolKind::Function,
        CXCursor_CXXMethod | CXCursor_Constructor | CXCursor_Destructor | CXCursor_ConversionFunction => {
            SymbolKind::Method
        }
        CXCursor_TypedefDecl | CXCursor_TypeAliasDecl | CXCursor_TypeAliasTemplateDecl => SymbolKind::Typedef,
        CXCursor_MacroDefinition => SymbolKind::Macro,
        _ => SymbolKind::Type,
    }
}

struct FindReferencingChild {
    target: CXCursor,
    found: CXCursor,
    has_found: bool,
}

extern "C" fn find_referencing_child_visitor(c: CXCursor, _parent: CXCursor, data: CXClientData) -> CXChildVisitResult {
    unsafe {
        let d = &mut *(data as *mut FindReferencingChild);
        let k = clang_getCursorKind(c);
        if k == CXCursor_DeclRefExpr || k == CXCursor_MemberRefExpr {
            let r = clang_getCursorReferenced(c);
            if clang_Cursor_isNull(r) == 0 && clang_equalCursors(r, d.target) != 0 {
                d.found = c;
                d.has_found = true;
                return CXChildVisit_Break;
            }
        }
        CXChildVisit_Continue
    }
}

extern "C" fn find_child_of_kind_visitor(c: CXCursor, _parent: CXCursor, data: CXClientData) -> CXChildVisitResult {
    unsafe {
        let out = &mut *(data as *mut CXCursor);
        if clang_getCursorKind(c) == CXCursor_Destructor {
            *out = c;
            return CXChildVisit_Break;
        }
        CXChildVisit_Continue
    }
}

extern "C" fn first_child_visitor(child: CXCursor, _parent: CXCursor, data: CXClientData) -> CXChildVisitResult {
    unsafe {
        let out = &mut *(data as *mut (CXCursor, bool));
        out.0 = child;
        out.1 = true;
    }
    CXChildVisit_Break
}

/// (file pointer, offset) dedup key for claimed call locations.
type LocKey = (usize, u32);

pub struct AstWalker<'a> {
    tu: CXTranslationUnit,
    files: &'a mut FileRegistry,
    sink: &'a mut dyn FactSink,
    symbol_stack: Vec<LocalId>,
    current_file: CXFile,
    claimed_locations: HashSet<LocKey>,
}

/// RAII frame that pushes/pops the enclosing-symbol stack.
struct ContextFrame<'a, 'b> {
    walker: &'a mut AstWalker<'b>,
    pushed: bool,
}

impl<'a, 'b> ContextFrame<'a, 'b> {
    fn new(walker: &'a mut AstWalker<'b>, sym: LocalId) -> ContextFrame<'a, 'b> {
        let pushed = sym != K_NO_LOCAL;
        if pushed {
            walker.symbol_stack.push(sym);
        }
        ContextFrame { walker, pushed }
    }
}

impl<'a, 'b> Drop for ContextFrame<'a, 'b> {
    fn drop(&mut self) {
        if self.pushed {
            self.walker.symbol_stack.pop();
        }
    }
}

extern "C" fn trampoline(cursor: CXCursor, parent: CXCursor, data: CXClientData) -> CXChildVisitResult {
    unsafe {
        let walker = &mut *(data as *mut AstWalker);
        walker.visit(cursor, parent)
    }
}

extern "C" fn inclusion_trampoline(included: CXFile, stack: *mut CXSourceLocation, len: u32, data: CXClientData) {
    unsafe {
        let walker = &mut *(data as *mut AstWalker);
        walker.handle_inclusion(included, stack, len);
    }
}

impl<'a> AstWalker<'a> {
    pub fn new(tu: CXTranslationUnit, files: &'a mut FileRegistry, sink: &'a mut dyn FactSink) -> AstWalker<'a> {
        AstWalker {
            tu,
            files,
            sink,
            symbol_stack: Vec::new(),
            current_file: std::ptr::null_mut(),
            claimed_locations: HashSet::new(),
        }
    }

    pub fn run(&mut self) {
        self.process_inclusions();
        let root = unsafe { clang_getTranslationUnitCursor(self.tu) };
        unsafe { clang_visitChildren(root, trampoline, self as *mut AstWalker as CXClientData) };
    }

    // --- libclang file resolution (feeds the pure-policy FileRegistry) ------

    fn file_key(file: CXFile) -> FileKey {
        let mut id = CXFileUniqueID { data: [0u64; 3] };
        if unsafe { clang_getFileUniqueID(file, &mut id) } != 0 {
            return FileKey { a: file as usize as u64, b: 0, c: 0 };
        }
        FileKey { a: id.data[0], b: id.data[1], c: id.data[2] }
    }

    fn file_is_system(&self, file: CXFile) -> bool {
        unsafe { clang_Location_isInSystemHeader(clang_getLocationForOffset(self.tu, file, 0)) != 0 }
    }

    fn file_content_hash(&self, file: CXFile) -> String {
        let mut size: usize = 0;
        let contents = unsafe { clang_getFileContents(self.tu, file, &mut size) };
        if contents.is_null() {
            return String::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(contents as *const u8, size) };
        content_hash(bytes)
    }

    fn file_info(&mut self, file: CXFile) -> FileInfo {
        let key = Self::file_key(file);
        let path = file_name(file);
        let system = self.file_is_system(file);
        let content_hash = if system {
            None
        } else {
            Some(self.file_content_hash(file))
        };
        self.files.info_for(key, &path, system, content_hash).clone()
    }

    // --- File rows + INCLUDE edges ------------------------------------------

    fn process_inclusions(&mut self) {
        unsafe {
            clang_getInclusions(self.tu, inclusion_trampoline, self as *mut AstWalker as CXClientData);
        }
    }

    fn handle_inclusion(&mut self, included_file: CXFile, stack: *mut CXSourceLocation, include_len: u32) {
        let mut includer_file: CXFile = std::ptr::null_mut();
        if include_len > 0 {
            let mut f: CXFile = std::ptr::null_mut();
            let mut l: u32 = 0;
            let mut col: u32 = 0;
            let mut off: u32 = 0;
            unsafe { clang_getFileLocation(*stack, &mut f, &mut l, &mut col, &mut off) };
            includer_file = f;
        }

        let mut includer_indexed = false;
        if !includer_file.is_null() {
            includer_indexed = self.file_info(includer_file).indexed;
            if includer_indexed {
                // Mirrors the C++ markDirectIncludeOfIndexed -> mutableInfoFor:
                // the included file's full info (including its content hash
                // when indexed) is resolved NOW, at first sight of the key.
                let key = Self::file_key(included_file);
                let path = file_name(included_file);
                let system = self.file_is_system(included_file);
                let content_hash = if system { None } else { Some(self.file_content_hash(included_file)) };
                self.files.mark_direct_include_of_indexed(key, &path, system, content_hash);
            }
        }

        if !self.files.should_register(Self::file_key(included_file)) {
            return;
        }
        self.file_fact_id(included_file);

        if !includer_file.is_null() && includer_indexed {
            let includer_sym = self.file_symbol_id(includer_file);
            let included_sym = self.file_symbol_id(included_file);
            let ref_id = self.sink.reference(includer_sym, included_sym, ReferenceKind::Include);

            let mut loc_file: CXFile = std::ptr::null_mut();
            let mut line: u32 = 0;
            let mut col: u32 = 0;
            let mut off: u32 = 0;
            unsafe { clang_getFileLocation(*stack, &mut loc_file, &mut line, &mut col, &mut off) };
            if !loc_file.is_null() {
                let range = SourceRange::new(self.file_fact_id(loc_file), line, col, line, col);
                self.sink.occurrence(ElementClass::Reference, ref_id, range, LocationKind::Token);
            }
        }
    }

    // --- Cursor dispatch -----------------------------------------------------

    fn cursor_file(c: CXCursor) -> CXFile {
        let loc = unsafe { clang_getCursorLocation(c) };
        let mut file: CXFile = std::ptr::null_mut();
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut off: u32 = 0;
        unsafe { clang_getFileLocation(loc, &mut file, &mut line, &mut col, &mut off) };
        file
    }

    fn visit(&mut self, c: CXCursor, _parent: CXCursor) -> CXChildVisitResult {
        let file = Self::cursor_file(c);
        if file.is_null() {
            return CXChildVisit_Continue;
        }
        if !self.file_info(file).indexed {
            return CXChildVisit_Continue;
        }
        self.current_file = file;

        match unsafe { clang_getCursorKind(c) } {
            CXCursor_Namespace
            | CXCursor_ClassDecl
            | CXCursor_StructDecl
            | CXCursor_UnionDecl
            | CXCursor_EnumDecl
            | CXCursor_ClassTemplate
            | CXCursor_ClassTemplatePartialSpecialization => self.visit_scope_decl(c),

            CXCursor_FunctionDecl
            | CXCursor_FunctionTemplate
            | CXCursor_CXXMethod
            | CXCursor_Constructor
            | CXCursor_Destructor
            | CXCursor_ConversionFunction => self.visit_function_like_decl(c),

            CXCursor_TypedefDecl | CXCursor_TypeAliasDecl | CXCursor_TypeAliasTemplateDecl | CXCursor_EnumConstantDecl | CXCursor_FieldDecl => {
                self.handle_simple_decl(c);
                CXChildVisit_Recurse
            }

            CXCursor_VarDecl => {
                self.handle_var_decl(c);
                CXChildVisit_Recurse
            }

            CXCursor_ParmDecl | CXCursor_TemplateTypeParameter | CXCursor_NonTypeTemplateParameter | CXCursor_TemplateTemplateParameter => {
                self.handle_local_decl(c);
                CXChildVisit_Recurse
            }

            CXCursor_MacroDefinition => {
                self.handle_macro_definition(c);
                CXChildVisit_Continue
            }

            CXCursor_MacroExpansion => {
                self.handle_macro_expansion(c);
                CXChildVisit_Continue
            }

            CXCursor_CallExpr => {
                self.handle_call_expr(c);
                CXChildVisit_Recurse
            }

            CXCursor_CXXDeleteExpr => {
                self.handle_delete_expr(c);
                CXChildVisit_Recurse
            }

            CXCursor_DeclRefExpr | CXCursor_MemberRefExpr => {
                self.handle_decl_or_member_ref(c);
                CXChildVisit_Recurse
            }

            CXCursor_TypeRef | CXCursor_TemplateRef => {
                self.handle_type_or_template_ref(c);
                CXChildVisit_Recurse
            }

            CXCursor_NamespaceRef => {
                self.handle_namespace_ref(c);
                CXChildVisit_Recurse
            }

            CXCursor_CXXBaseSpecifier => {
                self.handle_base_specifier(c);
                CXChildVisit_Continue
            }

            _ => CXChildVisit_Recurse,
        }
    }

    // --- Scope-introducing decls --------------------------------------------

    fn visit_scope_decl(&mut self, c: CXCursor) -> CXChildVisitResult {
        let id = self.ensure_symbol(c);
        self.mark_declared_here(id);

        self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_Cursor_getSpellingNameRange(c, 0, 0) }, LocationKind::Token);
        if unsafe { clang_isCursorDefinition(c) } != 0 {
            self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_getCursorExtent(c) }, LocationKind::Scope);
        }

        {
            let frame = ContextFrame::new(self, id);
            unsafe { clang_visitChildren(c, trampoline, (frame.walker as *mut AstWalker) as CXClientData) };
        }
        CXChildVisit_Continue
    }

    fn visit_function_like_decl(&mut self, c: CXCursor) -> CXChildVisitResult {
        let id = self.ensure_symbol(c);
        self.mark_declared_here(id);

        self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_Cursor_getSpellingNameRange(c, 0, 0) }, LocationKind::Token);
        if unsafe { clang_isCursorDefinition(c) } != 0 {
            self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_getCursorExtent(c) }, LocationKind::Scope);
        } else {
            self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_getCursorExtent(c) }, LocationKind::Signature);
        }

        let kind = unsafe { clang_getCursorKind(c) };
        if kind == CXCursor_CXXMethod || kind == CXCursor_Destructor {
            let mut overridden: *mut CXCursor = std::ptr::null_mut();
            let mut count: u32 = 0;
            unsafe { clang_getOverriddenCursors(c, &mut overridden, &mut count) };
            for i in 0..count {
                if !self.range_file_indexed(Self::spelling_range_or_point(c)) {
                    continue;
                }
                let target_id = self.ensure_symbol(unsafe { *overridden.add(i as usize) });
                let ref_id = self.sink.reference(id, target_id, ReferenceKind::Override);
                self.emit_occurrence_for_range(ElementClass::Reference, ref_id, Self::spelling_range_or_point(c), LocationKind::Token);
            }
            if !overridden.is_null() {
                unsafe { clang_disposeOverriddenCursors(overridden) };
            }
        }

        {
            let frame = ContextFrame::new(self, id);
            unsafe { clang_visitChildren(c, trampoline, (frame.walker as *mut AstWalker) as CXClientData) };
        }
        CXChildVisit_Continue
    }

    // --- Simple decls --------------------------------------------------------

    fn handle_simple_decl(&mut self, c: CXCursor) {
        let id = self.ensure_symbol(c);
        self.mark_declared_here(id);
        self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_Cursor_getSpellingNameRange(c, 0, 0) }, LocationKind::Token);
    }

    fn handle_var_decl(&mut self, c: CXCursor) {
        if classify_var_decl(c) == VarDeclKind::Local {
            self.handle_local_decl(c);
            return;
        }
        self.handle_simple_decl(c);
    }

    fn handle_local_decl(&mut self, c: CXCursor) {
        let name = build_local_symbol_name(c);
        let id = self.sink.local_symbol(name);
        self.emit_occurrence_for_range(ElementClass::LocalSymbol, id, unsafe { clang_Cursor_getSpellingNameRange(c, 0, 0) }, LocationKind::LocalSymbol);
    }

    // --- Macros --------------------------------------------------------------

    fn handle_macro_definition(&mut self, c: CXCursor) {
        if unsafe { clang_Cursor_isMacroBuiltin(c) } != 0 {
            return;
        }
        let id = self.ensure_symbol(c);
        self.mark_declared_here(id);
        self.emit_occurrence_for_range(ElementClass::Symbol, id, unsafe { clang_getCursorExtent(c) }, LocationKind::Token);
    }

    fn handle_macro_expansion(&mut self, c: CXCursor) {
        let referenced = unsafe { clang_getCursorReferenced(c) };
        if unsafe { clang_Cursor_isNull(referenced) } != 0 || unsafe { clang_Cursor_isMacroBuiltin(referenced) } != 0 {
            return;
        }

        let file = Self::cursor_file(c);
        if file.is_null() {
            return;
        }

        let range = unsafe { clang_getCursorExtent(c) };
        if !self.range_file_indexed(range) {
            return;
        }

        let ctx = self.file_symbol_id(file);
        let target_id = self.ensure_symbol(referenced);
        let ref_id = self.sink.reference(ctx, target_id, ReferenceKind::MacroUsage);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    // --- Calls -----------------------------------------------------------------

    fn find_call_location(&mut self, call_expr: CXCursor, callee: CXCursor) -> CXSourceRange {
        if unsafe { clang_Cursor_isNull(callee) } == 0 {
            let mut data = FindReferencingChild { target: callee, found: unsafe { clang_getNullCursor() }, has_found: false };
            unsafe {
                clang_visitChildren(call_expr, find_referencing_child_visitor, &mut data as *mut FindReferencingChild as CXClientData);
            }
            if data.has_found {
                let r = unsafe { clang_Cursor_getSpellingNameRange(data.found, 0, 0) };
                if unsafe { clang_Range_isNull(r) } == 0 {
                    return r;
                }
            }
        }

        let self_range = unsafe { clang_Cursor_getSpellingNameRange(call_expr, 0, 0) };
        if unsafe { clang_Range_isNull(self_range) } == 0 {
            return self_range;
        }

        let loc = unsafe { clang_getCursorLocation(call_expr) };
        unsafe { clang_getRange(loc, loc) }
    }

    fn claim_location(&mut self, range: CXSourceRange) {
        let loc = unsafe { clang_getRangeStart(range) };
        let mut f: CXFile = std::ptr::null_mut();
        let mut l: u32 = 0;
        let mut c: u32 = 0;
        let mut off: u32 = 0;
        unsafe { clang_getFileLocation(loc, &mut f, &mut l, &mut c, &mut off) };
        self.claimed_locations.insert((f as usize, off));
    }

    fn is_claimed_by_call(&self, range: CXSourceRange) -> bool {
        let loc = unsafe { clang_getRangeStart(range) };
        let mut f: CXFile = std::ptr::null_mut();
        let mut l: u32 = 0;
        let mut c: u32 = 0;
        let mut off: u32 = 0;
        unsafe { clang_getFileLocation(loc, &mut f, &mut l, &mut c, &mut off) };
        self.claimed_locations.contains(&(f as usize, off))
    }

    fn handle_call_expr(&mut self, c: CXCursor) {
        let callee = unsafe { clang_getCursorReferenced(c) };
        let range = self.find_call_location(c, callee);
        self.claim_location(range);
        if !self.range_file_indexed(range) {
            return;
        }

        let ctx = self.current_context();
        if unsafe { clang_Cursor_isNull(callee) } != 0 {
            let ref_id = self.sink.unsolved_reference(ctx, ReferenceKind::Call);
            self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
            return;
        }

        let target_id = self.ensure_symbol(callee);
        let ref_id = self.sink.reference(ctx, target_id, ReferenceKind::Call);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    fn handle_delete_expr(&mut self, c: CXCursor) {
        // First child = the operand expression.
        let first_found: CXCursor = unsafe { clang_getNullCursor() };
        let first_has = false;
        unsafe {
            clang_visitChildren(
                c,
                first_child_visitor,
                &mut (first_found, first_has) as *mut (CXCursor, bool) as CXClientData,
            );
        }
        if !first_has {
            return;
        }
        let operand = first_found;

        let op_type = unsafe { clang_getCursorType(operand) };
        let pointee = unsafe { clang_getPointeeType(op_type) };
        let record_decl = unsafe { clang_getTypeDeclaration(pointee) };
        if unsafe { clang_Cursor_isNull(record_decl) } != 0 {
            return;
        }

        let mut dtor: CXCursor = unsafe { clang_getNullCursor() };
        unsafe {
            clang_visitChildren(record_decl, find_child_of_kind_visitor, &mut dtor as *mut CXCursor as CXClientData);
        }
        if unsafe { clang_Cursor_isNull(dtor) } != 0 {
            return; // compiler-implicit destructor: documented gap
        }

        let range = Self::spelling_range_or_point(c);
        if !self.range_file_indexed(range) {
            return;
        }

        let target_id = self.ensure_symbol(dtor);
        let ctx = self.current_context();
        let ref_id = self.sink.reference(ctx, target_id, ReferenceKind::Call);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    // --- Usages / type usages / qualifiers / inheritance ----------------------

    fn handle_decl_or_member_ref(&mut self, c: CXCursor) {
        let range = unsafe { clang_getCursorReferenceNameRange(c, CXNameRange_WantSinglePiece, 0) };
        if !self.range_file_indexed(range) {
            return;
        }
        if self.is_claimed_by_call(range) {
            return;
        }

        let referenced = unsafe { clang_getCursorReferenced(c) };
        let ctx = self.current_context();

        if unsafe { clang_Cursor_isNull(referenced) } != 0 {
            let ref_id = self.sink.unsolved_reference(ctx, ReferenceKind::Usage);
            self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
            return;
        }

        if is_local_decl_kind(referenced) {
            let name = build_local_symbol_name(referenced);
            let id = self.sink.local_symbol(name);
            self.emit_occurrence_for_range(ElementClass::LocalSymbol, id, range, LocationKind::LocalSymbol);
            return;
        }

        let target_id = self.ensure_symbol(referenced);
        let ref_id = self.sink.reference(ctx, target_id, ReferenceKind::Usage);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    fn handle_type_or_template_ref(&mut self, c: CXCursor) {
        let referenced = unsafe { clang_getCursorReferenced(c) };
        let ctx = self.current_context();
        let range = Self::spelling_range_or_point(c);
        if !self.range_file_indexed(range) {
            return;
        }

        if unsafe { clang_Cursor_isNull(referenced) } != 0 {
            let ref_id = self.sink.unsolved_reference(ctx, ReferenceKind::TypeUsage);
            self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
            return;
        }

        let target_id = self.ensure_symbol(referenced);
        let ref_id = self.sink.reference(ctx, target_id, ReferenceKind::TypeUsage);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    fn handle_namespace_ref(&mut self, c: CXCursor) {
        let referenced = unsafe { clang_getCursorReferenced(c) };
        if unsafe { clang_Cursor_isNull(referenced) } != 0 {
            return;
        }
        if !self.range_file_indexed(unsafe { clang_getCursorExtent(c) }) {
            return;
        }
        let target_id = self.ensure_symbol(referenced);
        self.emit_occurrence_for_range(ElementClass::Symbol, target_id, unsafe { clang_getCursorExtent(c) }, LocationKind::Qualifier);
    }

    fn handle_base_specifier(&mut self, c: CXCursor) {
        let derived_id = match self.symbol_stack.last() {
            Some(&id) => id,
            None => return,
        };

        let mut base = unsafe { clang_getCursorReferenced(c) };
        if unsafe { clang_Cursor_isNull(base) } != 0 {
            base = unsafe { clang_getTypeDeclaration(clang_getCursorType(c)) };
        }
        if unsafe { clang_Cursor_isNull(base) } != 0 {
            return;
        }

        let range = Self::spelling_range_or_point(c);
        if !self.range_file_indexed(range) {
            return;
        }

        let target_id = self.ensure_symbol(base);
        let ref_id = self.sink.reference(derived_id, target_id, ReferenceKind::Inheritance);
        self.emit_occurrence_for_range(ElementClass::Reference, ref_id, range, LocationKind::Token);
    }

    // --- Symbol registration --------------------------------------------------

    fn ensure_symbol(&mut self, cursor: CXCursor) -> LocalId {
        let canonical = unsafe { clang_getCanonicalCursor(cursor) };

        let mut chain: Vec<CXCursor> = Vec::new();
        let mut cur = canonical;
        while unsafe { clang_Cursor_isNull(cur) } == 0 && unsafe { clang_getCursorKind(cur) } != CXCursor_TranslationUnit {
            if unsafe { clang_getCursorKind(cur) } != CXCursor_LinkageSpec {
                chain.push(cur);
            }
            let parent = unsafe { clang_getCursorSemanticParent(cur) };
            cur = if unsafe { clang_Cursor_isNull(parent) } != 0 {
                parent
            } else {
                unsafe { clang_getCanonicalCursor(parent) }
            };
        }

        let mut leaf_id = K_NO_LOCAL;
        for cur in chain.iter().rev() {
            leaf_id = self.register_one(*cur);
        }
        leaf_id
    }

    fn register_one(&mut self, canonical_cursor: CXCursor) -> LocalId {
        let identity = build_symbol_identity(canonical_cursor);

        let fact = SymbolFact {
            key: identity.key,
            name: identity.name,
            kind: classify_kind(canonical_cursor),
            definition: if unsafe { clang_isCursorDefinition(canonical_cursor) } != 0 {
                DefinitionKind::Explicit
            } else {
                DefinitionKind::None
            },
            decl_files: Vec::new(),
        };
        self.sink.symbol(fact)
    }

    fn mark_declared_here(&mut self, id: LocalId) {
        if id == K_NO_LOCAL {
            return;
        }
        self.sink.refine_symbol(id, SymbolKind::Unknown, DefinitionKind::Explicit);
        if !self.current_file.is_null() {
            let file_id = self.file_fact_id(self.current_file);
            self.sink.declared_in(id, file_id);
        }
    }

    fn spelling_range_or_point(c: CXCursor) -> CXSourceRange {
        let range = unsafe { clang_Cursor_getSpellingNameRange(c, 0, 0) };
        if unsafe { clang_Range_isNull(range) } != 0 {
            let loc = unsafe { clang_getCursorLocation(c) };
            return unsafe { clang_getRange(loc, loc) };
        }
        range
    }

    fn file_fact_id(&mut self, file: CXFile) -> LocalId {
        let info = self.file_info(file);
        let fact = FileFact {
            path: info.path,
            language: "cpp".to_string(),
            indexed: info.indexed,
            system: info.system,
            content_hash: info.content_hash,
        };
        self.sink.file(fact)
    }

    fn file_symbol_id(&mut self, file: CXFile) -> LocalId {
        self.file_fact_id(file);
        let info = self.file_info(file);

        let fact = SymbolFact {
            key: info.path.clone(),
            name: Name {
                delimiter: "/".to_string(),
                elements: vec![NameElement { prefix: String::new(), name: info.path, postfix: String::new() }],
            },
            kind: SymbolKind::File,
            definition: DefinitionKind::None,
            decl_files: Vec::new(),
        };
        self.sink.symbol(fact)
    }

    fn current_context(&mut self) -> LocalId {
        if let Some(&id) = self.symbol_stack.last() {
            return id;
        }
        if !self.current_file.is_null() {
            return self.file_symbol_id(self.current_file);
        }
        K_NO_LOCAL
    }

    fn range_file_indexed(&mut self, range: CXSourceRange) -> bool {
        if unsafe { clang_Range_isNull(range) } != 0 {
            return false;
        }
        let mut file: CXFile = std::ptr::null_mut();
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut off: u32 = 0;
        unsafe { clang_getFileLocation(clang_getRangeStart(range), &mut file, &mut line, &mut col, &mut off) };
        if file.is_null() {
            return false;
        }
        self.file_info(file).indexed
    }

    fn range_to_model_range(&mut self, range: CXSourceRange, out: &mut SourceRange) -> bool {
        if unsafe { clang_Range_isNull(range) } != 0 {
            return false;
        }
        let start_loc = unsafe { clang_getRangeStart(range) };
        let end_loc = unsafe { clang_getRangeEnd(range) };

        let mut start_file: CXFile = std::ptr::null_mut();
        let mut sl: u32 = 0;
        let mut sc: u32 = 0;
        let mut so: u32 = 0;
        unsafe { clang_getFileLocation(start_loc, &mut start_file, &mut sl, &mut sc, &mut so) };
        if start_file.is_null() {
            return false;
        }

        let mut end_file: CXFile = std::ptr::null_mut();
        let mut el: u32 = 0;
        let mut ec: u32 = 0;
        let mut eo: u32 = 0;
        unsafe { clang_getFileLocation(end_loc, &mut end_file, &mut el, &mut ec, &mut eo) };

        out.file = self.file_fact_id(start_file);
        out.start_line = sl;
        out.start_col = sc;
        out.end_line = if !end_file.is_null() { el } else { sl };
        out.end_col = if !end_file.is_null() { ec } else { sc };
        true
    }

    fn emit_occurrence_for_range(&mut self, cls: ElementClass, element: LocalId, range: CXSourceRange, kind: LocationKind) {
        if element == K_NO_LOCAL {
            return;
        }
        if !self.range_file_indexed(range) {
            return;
        }
        let mut model_range = SourceRange::default();
        if !self.range_to_model_range(range, &mut model_range) {
            return;
        }
        self.sink.occurrence(cls, element, model_range, kind);
    }
}