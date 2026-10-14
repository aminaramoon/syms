//! Builds backend-neutral `symhow::model::Name` hierarchies and SymbolKeys
//! from libclang cursors. Mirrors `symhow::index::NameBuilder`.

// clang-sys names its cursor-kind/flag constants after clang's CamelCase
// spellings (e.g. `CXCursor_StructDecl`), which trips non_upper_case_globals
// when used in match patterns.
#![allow(non_upper_case_globals)]

use std::cell::RefCell;
use std::collections::HashMap;

use clang_sys::*;
use regex::Regex;

use crate::common::paths;
use crate::model::{Name, NameElement};

use super::clangutil::{cx_string_to_string, file_name, fnv1a64, hex8};

pub struct SymbolIdentity {
    pub key: String,
    pub name: Name,
}

fn basename_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

thread_local! {
    static NORMALIZED_FILE_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Normalized file path for a `CXFile`, memoized per thread.
fn normalized_file_name(file: CXFile) -> String {
    if file.is_null() {
        return String::new();
    }
    let raw = file_name(file);
    NORMALIZED_FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(hit) = cache.get(&raw) {
            return hit.clone();
        }
        let normalized = paths::normalize(&raw);
        cache.insert(raw.clone(), normalized.clone());
        normalized
    })
}

fn cursor_file_and_line(c: CXCursor) -> (String, u32) {
    let loc = unsafe { clang_getCursorLocation(c) };
    let mut file: CXFile = std::ptr::null_mut();
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    let mut offset: u32 = 0;
    unsafe { clang_getFileLocation(loc, &mut file, &mut line, &mut col, &mut offset) };
    (normalized_file_name(file), line)
}

fn kind_word_for(kind: CXCursorKind) -> &'static str {
    match kind {
        CXCursor_StructDecl => "struct",
        CXCursor_UnionDecl => "union",
        CXCursor_EnumDecl => "enum",
        CXCursor_ClassDecl | CXCursor_ClassTemplate | CXCursor_ClassTemplatePartialSpecialization => "class",
        _ => "record",
    }
}

fn anonymous_record_element(c: CXCursor) -> NameElement {
    let (file, line) = cursor_file_and_line(c);
    NameElement {
        prefix: String::new(),
        name: format!(
            "(anonymous {} at {}:{})",
            kind_word_for(unsafe { clang_getCursorKind(c) }),
            basename_of(&file),
            line
        ),
        postfix: String::new(),
    }
}

fn namespace_element(c: CXCursor) -> NameElement {
    let spelling = cx_string_to_string(unsafe { clang_getCursorSpelling(c) });
    if spelling.is_empty() {
        NameElement { prefix: String::new(), name: "(anonymous namespace)".to_string(), postfix: String::new() }
    } else {
        NameElement { prefix: String::new(), name: spelling, postfix: String::new() }
    }
}

fn plain_element(c: CXCursor) -> NameElement {
    NameElement { prefix: String::new(), name: cx_string_to_string(unsafe { clang_getCursorSpelling(c) }), postfix: String::new() }
}

fn is_record_like(kind: CXCursorKind) -> bool {
    kind == CXCursor_StructDecl
        || kind == CXCursor_ClassDecl
        || kind == CXCursor_UnionDecl
        || kind == CXCursor_ClassTemplate
        || kind == CXCursor_ClassTemplatePartialSpecialization
}

fn is_function_like(kind: CXCursorKind) -> bool {
    kind == CXCursor_FunctionDecl
        || kind == CXCursor_FunctionTemplate
        || kind == CXCursor_CXXMethod
        || kind == CXCursor_Constructor
        || kind == CXCursor_Destructor
        || kind == CXCursor_ConversionFunction
}

fn function_element(c: CXCursor) -> NameElement {
    let mut el = NameElement::default();
    let kind = unsafe { clang_getCursorKind(c) };

    if kind != CXCursor_Constructor && kind != CXCursor_Destructor {
        let result_spelling = cx_string_to_string(unsafe { clang_getTypeSpelling(clang_getCursorResultType(c)) });
        if !result_spelling.is_empty() {
            el.prefix = format!("{} ", result_spelling);
        }
    }

    el.name = cx_string_to_string(unsafe { clang_getCursorSpelling(c) });

    let mut params = String::from("(");
    let num_args = unsafe { clang_Cursor_getNumArguments(c) };
    for i in 0..num_args {
        if i != 0 {
            params.push_str(", ");
        }
        let arg_cursor = unsafe { clang_Cursor_getArgument(c, i as u32) };
        params.push_str(&cx_string_to_string(unsafe { clang_getTypeSpelling(clang_getCursorType(arg_cursor)) }));
    }
    params.push(')');

    if unsafe { clang_CXXMethod_isConst(c) } != 0 {
        params.push_str(" const");
    }
    let rq = unsafe { clang_Type_getCXXRefQualifier(clang_getCursorType(c)) };
    if rq == CXRefQualifier_LValue {
        params.push_str(" &");
    } else if rq == CXRefQualifier_RValue {
        params.push_str(" &&");
    }

    el.postfix = params;
    el
}

fn element_for_cursor(c: CXCursor) -> NameElement {
    let kind = unsafe { clang_getCursorKind(c) };

    if kind == CXCursor_Namespace {
        return namespace_element(c);
    }
    if is_record_like(kind) || kind == CXCursor_EnumDecl {
        let spelling = cx_string_to_string(unsafe { clang_getCursorSpelling(c) });
        if spelling.is_empty() {
            return anonymous_record_element(c);
        }
        return NameElement { prefix: String::new(), name: spelling, postfix: String::new() };
    }
    if is_function_like(kind) {
        return function_element(c);
    }
    plain_element(c)
}

fn ancestor_chain_root_to_leaf(cursor: CXCursor) -> Vec<CXCursor> {
    let mut chain = Vec::new();
    let mut cur = unsafe { clang_getCursorSemanticParent(cursor) };
    while unsafe { clang_Cursor_isNull(cur) } == 0 && unsafe { clang_getCursorKind(cur) } != CXCursor_TranslationUnit {
        let canonical_cur = unsafe { clang_getCanonicalCursor(cur) };
        if unsafe { clang_getCursorKind(canonical_cur) } != CXCursor_LinkageSpec {
            chain.push(canonical_cur);
        }
        cur = unsafe { clang_getCursorSemanticParent(canonical_cur) };
    }
    chain.reverse();
    chain
}

fn has_anonymous_namespace_ancestor(canonical: CXCursor) -> bool {
    let mut cur = unsafe { clang_getCursorSemanticParent(canonical) };
    while unsafe { clang_Cursor_isNull(cur) } == 0 && unsafe { clang_getCursorKind(cur) } != CXCursor_TranslationUnit {
        if unsafe { clang_getCursorKind(cur) } == CXCursor_Namespace
            && cx_string_to_string(unsafe { clang_getCursorSpelling(cur) }).is_empty()
        {
            return true;
        }
        cur = unsafe { clang_getCursorSemanticParent(cur) };
    }
    false
}

fn is_local_scope(canonical: CXCursor) -> bool {
    let linkage = unsafe { clang_getCursorLinkage(canonical) };
    let spelling = cx_string_to_string(unsafe { clang_getCursorSpelling(canonical) });
    let parent = unsafe { clang_getCursorSemanticParent(canonical) };
    let parent_is_tu = unsafe { clang_Cursor_isNull(parent) } == 0 && unsafe { clang_getCursorKind(parent) } == CXCursor_TranslationUnit;

    linkage == CXLinkage_Internal
        || has_anonymous_namespace_ancestor(canonical)
        || (spelling == "main" && parent_is_tu)
}

fn disambiguator_for(canonical: CXCursor) -> String {
    let loc = unsafe { clang_getCursorLocation(canonical) };
    let mut file: CXFile = std::ptr::null_mut();
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    let mut offset: u32 = 0;
    unsafe { clang_getFileLocation(loc, &mut file, &mut line, &mut col, &mut offset) };
    let decl_file = normalized_file_name(file);
    format!(" [{}#{}]", basename_of(&decl_file), hex8(fnv1a64(&decl_file)))
}

fn normalize_lambda_paths_in_text(text: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\(lambda at ([^():]+):(\d+):(\d+)\)").unwrap());
    let mut result = String::with_capacity(text.len());
    let mut last_pos = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(0).unwrap();
        result.push_str(&text[last_pos..m.start()]);
        result.push_str(&format!(
            "(lambda at {}:{}:{})",
            paths::normalize(&caps[1]),
            &caps[2],
            &caps[3]
        ));
        last_pos = m.end();
    }
    result.push_str(&text[last_pos..]);
    result
}

fn identity_element(canonical: CXCursor) -> NameElement {
    let mut el = element_for_cursor(canonical);
    if is_local_scope(canonical) {
        el.postfix.push_str(&disambiguator_for(canonical));
    }
    el.prefix = normalize_lambda_paths_in_text(&el.prefix);
    el.name = normalize_lambda_paths_in_text(&el.name);
    el.postfix = normalize_lambda_paths_in_text(&el.postfix);
    el
}

/// Builds the `Name` hierarchy for `cursor` (canonicalized internally).
pub fn build_name(cursor: CXCursor) -> Name {
    let canonical = unsafe { clang_getCanonicalCursor(cursor) };

    let mut name = Name::new("::");
    for ancestor in ancestor_chain_root_to_leaf(canonical) {
        name.elements.push(identity_element(ancestor));
    }
    name.elements.push(identity_element(canonical));
    name
}

/// Computes the SymbolKey + Name for `cursor`.
pub fn build_symbol_identity(cursor: CXCursor) -> SymbolIdentity {
    let canonical = unsafe { clang_getCanonicalCursor(cursor) };

    let mut identity = SymbolIdentity { key: String::new(), name: build_name(canonical) };

    let mut usr = cx_string_to_string(unsafe { clang_getCursorUSR(canonical) });
    if usr.is_empty() {
        let no_usr_loc = unsafe { clang_getCursorLocation(canonical) };
        let mut file: CXFile = std::ptr::null_mut();
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut offset: u32 = 0;
        unsafe { clang_getFileLocation(no_usr_loc, &mut file, &mut line, &mut col, &mut offset) };
        usr = format!(
            "no-usr:{}:{}:{}",
            unsafe { clang_getCursorKind(canonical) } as i32,
            normalized_file_name(file),
            offset
        );
    }

    identity.key = usr;
    if is_local_scope(canonical) {
        let loc = unsafe { clang_getCursorLocation(canonical) };
        let mut file: CXFile = std::ptr::null_mut();
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut offset: u32 = 0;
        unsafe { clang_getFileLocation(loc, &mut file, &mut line, &mut col, &mut offset) };
        identity.key.push('\u{1f}');
        identity.key.push_str(&normalized_file_name(file));
    }

    identity
}

/// Name for a local symbol: "<canonical decl file path>:<line>:<col>".
pub fn build_local_symbol_name(cursor: CXCursor) -> String {
    let canonical = unsafe { clang_getCanonicalCursor(cursor) };
    let loc = unsafe { clang_getCursorLocation(canonical) };
    let mut file: CXFile = std::ptr::null_mut();
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    let mut offset: u32 = 0;
    unsafe { clang_getFileLocation(loc, &mut file, &mut line, &mut col, &mut offset) };
    let path = if file.is_null() { "<unknown>".to_string() } else { normalized_file_name(file) };
    format!("{}:{}:{}", path, line, col)
}