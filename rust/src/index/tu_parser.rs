//! Parses a single translation unit with libclang. Mirrors
//! `symhow::index::TuParser`.

use clang_sys::*;

use super::clangutil::{cx_string_to_string, file_name, ScopedIndex, ScopedTu};

#[derive(Debug, Clone, Default)]
pub struct DiagInfo {
    pub message: String,
    pub fatal: bool,
    pub has_location: bool,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

pub struct ParsedTu {
    pub index: ScopedIndex,
    pub tu: ScopedTu,
    pub ok: bool,
    pub diagnostics: Vec<DiagInfo>,
    pub diag_errors: u32,
    pub diag_fatal: u32,
}

fn collect_diagnostics(tu: CXTranslationUnit, result: &mut ParsedTu) {
    let main_file = cx_string_to_string(unsafe { clang_getTranslationUnitSpelling(tu) });

    let num_diags = unsafe { clang_getNumDiagnostics(tu) };
    for i in 0..num_diags {
        let diag = unsafe { clang_getDiagnostic(tu, i) };
        let severity = unsafe { clang_getDiagnosticSeverity(diag) };
        if (severity as i32) < CXDiagnostic_Error as i32 {
            unsafe { clang_disposeDiagnostic(diag) };
            continue;
        }

        let mut info = DiagInfo {
            message: cx_string_to_string(unsafe {
                clang_formatDiagnostic(diag, clang_defaultDiagnosticDisplayOptions())
            }),
            fatal: severity == CXDiagnostic_Fatal,
            has_location: false,
            file: String::new(),
            line: 1,
            col: 1,
        };

        let loc = unsafe { clang_getDiagnosticLocation(diag) };
        let mut file: CXFile = std::ptr::null_mut();
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut offset: u32 = 0;
        unsafe { clang_getExpansionLocation(loc, &mut file, &mut line, &mut col, &mut offset) };

        if !file.is_null() {
            info.has_location = true;
            info.file = file_name(file);
            info.line = line;
            info.col = col;
        } else {
            info.has_location = true;
            info.file = main_file.clone();
            info.line = 1;
            info.col = 1;
        }

        if severity == CXDiagnostic_Fatal {
            result.diag_fatal += 1;
        } else {
            result.diag_errors += 1;
        }

        result.diagnostics.push(info);
        unsafe { clang_disposeDiagnostic(diag) };
    }
}

/// Parses one TU. `args` is the fully-sanitized argv EXCLUDING argv[0].
pub fn parse_tu(args: &[String]) -> ParsedTu {
    super::clangutil::ensure_libclang_loaded();

    let mut result = ParsedTu {
        index: ScopedIndex::new(),
        tu: ScopedTu::new(),
        ok: false,
        diagnostics: Vec::new(),
        diag_errors: 0,
        diag_fatal: 0,
    };

    result.index = ScopedIndex::from_raw(unsafe { clang_createIndex(0, 0) });

    let argv: Vec<std::ffi::CString> = args.iter().map(|a| std::ffi::CString::new(a.as_str()).unwrap_or_default()).collect();
    let argv_ptrs: Vec<*const std::ffi::c_char> = argv.iter().map(|a| a.as_ptr()).collect();

    let opts = CXTranslationUnit_DetailedPreprocessingRecord
        | CXTranslationUnit_KeepGoing
        | CXTranslationUnit_IgnoreNonErrorsFromIncludedFiles;

    let mut tu: CXTranslationUnit = std::ptr::null_mut();
    let ec = unsafe {
        clang_parseTranslationUnit2(
            result.index.get(),
            std::ptr::null(),
            argv_ptrs.as_ptr(),
            argv_ptrs.len() as i32,
            std::ptr::null_mut(),
            0,
            opts,
            &mut tu,
        )
    };

    result.tu = ScopedTu::from_raw(tu);
    result.ok = ec == CXError_Success && !tu.is_null();

    if !tu.is_null() {
        collect_diagnostics(tu, &mut result);
    }

    result
}