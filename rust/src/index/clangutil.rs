//! Small pure utility helpers shared by the index and notes layers. Mirrors
//! `symhow::index::clangutil` (the libclang RAII parts land with the index
//! walker; these pure functions have no libclang dependency).

/// FNV-1a 64-bit hash (well-known, public-domain algorithm).
pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 14695981039346656037; // offset basis
    for c in s.bytes() {
        h ^= c as u64;
        h = h.wrapping_mul(1099511628211); // FNV prime
    }
    h
}

/// Lower 8 hex digits (32 bits) of `v`, e.g. "3f9a12c7".
pub fn hex8(v: u64) -> String {
    let low = (v & 0xFFFF_FFFF) as u32;
    format!("{:08x}", low)
}

/// Full 16 lowercase hex digits (64 bits) of `v`.
pub fn hex16(v: u64) -> String {
    format!("{:016x}", v)
}

// --- libclang runtime loading ------------------------------------------------

/// clang-sys in `runtime` mode keeps the dlopen'd libclang in THREAD-LOCAL
/// storage: every thread that calls libclang (the main thread, and each -j
/// indexing worker) must load it once. Idempotent and cheap after the first
/// call (dlopen of an already-open library just refcounts).
pub fn ensure_libclang_loaded() {
    if clang_sys::get_library().is_none() {
        if let Err(e) = clang_sys::load() {
            panic!("failed to load libclang (is LIBCLANG_PATH set?): {}", e);
        }
    }
}

// --- libclang helpers (RAII/disposal) ---------------------------------------

use clang_sys::*;

/// Converts and disposes a `CXString` in one step.
pub fn cx_string_to_string(s: CXString) -> String {
    unsafe {
        let c = clang_getCString(s);
        let result = if c.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(c).to_string_lossy().into_owned()
        };
        clang_disposeString(s);
        result
    }
}

/// Absolute-ish file path for a `CXFile`, exactly as libclang reports it (no
/// realpath/normalization).
pub fn file_name(file: CXFile) -> String {
    if file.is_null() {
        return String::new();
    }
    cx_string_to_string(unsafe { clang_getFileName(file) })
}

/// Owns a `CXIndex`, disposing it on destruction.
pub struct ScopedIndex(CXIndex);

impl ScopedIndex {
    pub fn new() -> ScopedIndex {
        ScopedIndex(std::ptr::null_mut())
    }

    pub fn from_raw(idx: CXIndex) -> ScopedIndex {
        ScopedIndex(idx)
    }

    pub fn get(&self) -> CXIndex {
        self.0
    }
}

impl Default for ScopedIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedIndex {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { clang_disposeIndex(self.0) };
        }
    }
}

/// Owns a `CXTranslationUnit`, disposing it on destruction.
pub struct ScopedTu(CXTranslationUnit);

impl ScopedTu {
    pub fn new() -> ScopedTu {
        ScopedTu(std::ptr::null_mut())
    }

    pub fn from_raw(tu: CXTranslationUnit) -> ScopedTu {
        ScopedTu(tu)
    }

    pub fn get(&self) -> CXTranslationUnit {
        self.0
    }
}

impl Default for ScopedTu {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedTu {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { clang_disposeTranslationUnit(self.0) };
        }
    }
}