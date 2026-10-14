//! FFI bindings to the C++ SourcetrailDB writer, compiled by `build.rs` from
//! `ffi/symhow_srctrldb_bridge.cpp` + `third_party/sourcetraildb`. This is the
//! ONLY C++ code the Rust backend links against.

use std::ffi::{c_char, CStr, CString};

// --- C ABI types (must match ffi/symhow_srctrldb_bridge.h) -----------------

#[repr(C)]
pub struct SymhowNameElement {
    pub prefix: *const c_char,
    pub name: *const c_char,
    pub postfix: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SymhowSourceRange {
    pub file_id: i32,
    pub start_line: i32,
    pub start_column: i32,
    pub end_line: i32,
    pub end_column: i32,
}

/// ReferenceKind ordinals (must match third_party/sourcetraildb/include/ReferenceKind.h).
#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReferenceKindInt {
    TypeUsage = 0,
    Usage = 1,
    Call = 2,
    Inheritance = 3,
    Override = 4,
    TypeArgument = 5,
    TemplateSpecialization = 6,
    Include = 7,
    Import = 8,
    MacroUsage = 9,
    AnnotationUsage = 10,
}

#[repr(C)]
pub struct SymhowWriter {
    _private: [u8; 0],
}

// --- raw extern declarations -------------------------------------------------

extern "C" {
    fn symhow_writer_create() -> *mut SymhowWriter;
    fn symhow_writer_destroy(w: *mut SymhowWriter);
    fn symhow_writer_last_error(w: *mut SymhowWriter) -> *const c_char;

    fn symhow_writer_open(w: *mut SymhowWriter, path: *const c_char) -> bool;
    fn symhow_writer_close(w: *mut SymhowWriter) -> bool;
    fn symhow_writer_begin_transaction(w: *mut SymhowWriter) -> bool;
    fn symhow_writer_commit_transaction(w: *mut SymhowWriter) -> bool;
    fn symhow_writer_rollback_transaction(w: *mut SymhowWriter) -> bool;

    fn symhow_writer_record_symbol(
        w: *mut SymhowWriter,
        name_delimiter: *const c_char,
        elements: *const SymhowNameElement,
        count: i32,
    ) -> i32;
    fn symhow_writer_record_file(w: *mut SymhowWriter, path: *const c_char) -> i32;
    fn symhow_writer_record_file_language(w: *mut SymhowWriter, file_id: i32, language: *const c_char) -> bool;
    fn symhow_writer_record_local_symbol(w: *mut SymhowWriter, name: *const c_char) -> i32;
    fn symhow_writer_record_reference(
        w: *mut SymhowWriter,
        context_symbol_id: i32,
        referenced_symbol_id: i32,
        reference_kind: i32,
    ) -> i32;
    fn symhow_writer_record_symbol_location(w: *mut SymhowWriter, symbol_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_symbol_scope_location(w: *mut SymhowWriter, symbol_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_symbol_signature_location(w: *mut SymhowWriter, symbol_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_qualifier_location(w: *mut SymhowWriter, referenced_symbol_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_local_symbol_location(w: *mut SymhowWriter, local_symbol_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_reference_location(w: *mut SymhowWriter, reference_id: i32, location: SymhowSourceRange) -> bool;
    fn symhow_writer_record_reference_to_unsolved(
        w: *mut SymhowWriter,
        context_symbol_id: i32,
        reference_kind: i32,
        location: SymhowSourceRange,
    ) -> i32;
    fn symhow_writer_record_error(w: *mut SymhowWriter, message: *const c_char, fatal: bool, location: SymhowSourceRange) -> bool;

    fn symhow_serialize_name_hierarchy(
        name_delimiter: *const c_char,
        elements: *const SymhowNameElement,
        count: i32,
    ) -> *mut c_char;
    fn symhow_free_string(s: *mut c_char);
}

// --- safe wrapper ------------------------------------------------------------

/// Owns a `SymhowWriter*`. All path/name strings are converted to C strings
/// internally; the caller passes Rust `&str`.
pub struct Writer {
    raw: *mut SymhowWriter,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { raw: unsafe { symhow_writer_create() } }
    }

    pub fn last_error(&self) -> String {
        unsafe {
            let p = symhow_writer_last_error(self.raw);
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }

    pub fn open(&self, path: &str) -> bool {
        let c = CString::new(path).unwrap_or_default();
        unsafe { symhow_writer_open(self.raw, c.as_ptr()) }
    }

    pub fn close(&self) -> bool {
        unsafe { symhow_writer_close(self.raw) }
    }

    pub fn begin_transaction(&self) -> bool {
        unsafe { symhow_writer_begin_transaction(self.raw) }
    }

    pub fn commit_transaction(&self) -> bool {
        unsafe { symhow_writer_commit_transaction(self.raw) }
    }

    pub fn rollback_transaction(&self) -> bool {
        unsafe { symhow_writer_rollback_transaction(self.raw) }
    }

    pub fn record_symbol(&self, name_delimiter: &str, elements: &[SymhowNameElement]) -> i32 {
        unsafe {
            let delim = CString::new(name_delimiter).unwrap_or_default();
            symhow_writer_record_symbol(self.raw, delim.as_ptr(), elements.as_ptr(), elements.len() as i32)
        }
    }

    pub fn record_file(&self, path: &str) -> i32 {
        let c = CString::new(path).unwrap_or_default();
        unsafe { symhow_writer_record_file(self.raw, c.as_ptr()) }
    }

    pub fn record_file_language(&self, file_id: i32, language: &str) -> bool {
        let c = CString::new(language).unwrap_or_default();
        unsafe { symhow_writer_record_file_language(self.raw, file_id, c.as_ptr()) }
    }

    pub fn record_local_symbol(&self, name: &str) -> i32 {
        let c = CString::new(name).unwrap_or_default();
        unsafe { symhow_writer_record_local_symbol(self.raw, c.as_ptr()) }
    }

    pub fn record_reference(&self, context: i32, target: i32, kind: ReferenceKindInt) -> i32 {
        unsafe { symhow_writer_record_reference(self.raw, context, target, kind as i32) }
    }

    pub fn record_symbol_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_symbol_location(self.raw, id, r) }
    }

    pub fn record_symbol_scope_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_symbol_scope_location(self.raw, id, r) }
    }

    pub fn record_symbol_signature_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_symbol_signature_location(self.raw, id, r) }
    }

    pub fn record_qualifier_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_qualifier_location(self.raw, id, r) }
    }

    pub fn record_local_symbol_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_local_symbol_location(self.raw, id, r) }
    }

    pub fn record_reference_location(&self, id: i32, r: SymhowSourceRange) -> bool {
        unsafe { symhow_writer_record_reference_location(self.raw, id, r) }
    }

    pub fn record_reference_to_unsolved(&self, context: i32, kind: ReferenceKindInt, r: SymhowSourceRange) -> i32 {
        unsafe { symhow_writer_record_reference_to_unsolved(self.raw, context, kind as i32, r) }
    }

    pub fn record_error(&self, message: &str, fatal: bool, r: SymhowSourceRange) -> bool {
        let c = CString::new(message).unwrap_or_default();
        unsafe { symhow_writer_record_error(self.raw, c.as_ptr(), fatal, r) }
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe { symhow_writer_destroy(self.raw) }
    }
}

/// Serializes a name hierarchy to the Sourcetrail database string form (the
/// exact inverse of `symhow::db::name_decode::decode_serialized_name`).
pub fn serialize_name_hierarchy(name_delimiter: &str, elements: &[SymhowNameElement]) -> String {
    unsafe {
        let delim = CString::new(name_delimiter).unwrap_or_default();
        let p = symhow_serialize_name_hierarchy(delim.as_ptr(), elements.as_ptr(), elements.len() as i32);
        if p.is_null() {
            return String::new();
        }
        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
        symhow_free_string(p);
        s
    }
}