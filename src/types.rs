//! Core shared types: symbol kinds, language ids, symbol/file records.

use std::fmt;
use std::str::FromStr;

/// A language understood by symwhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lang {
    C,
    Cpp,
    Cuda,
    Rust,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::C, Lang::Cpp, Lang::Cuda, Lang::Rust];

    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Cuda => "cuda",
            Lang::Rust => "rust",
        }
    }

    /// Detect language from a file path (extension based).
    /// CUDA shares the C++ grammar, so .cu/.cuh map to Lang::Cuda.
    pub fn detect(path: &std::path::Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "c" => Lang::C,
            "h" => Lang::C, // heuristic; overridden by --lang when needed
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "inl" | "ipp" | "tpp"
            | "txx" | "ixx" | "cppm" | "ii" => Lang::Cpp,
            "cu" | "cuh" => Lang::Cuda,
            "rs" => Lang::Rust,
            _ => return None,
        })
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Lang {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "c" => Lang::C,
            "cpp" | "c++" | "cxx" => Lang::Cpp,
            "cuda" | "cu" => Lang::Cuda,
            "rust" | "rs" => Lang::Rust,
            other => anyhow::bail!("unknown language `{other}` (expected c, cpp, cuda, rust)"),
        })
    }
}

/// Normalized symbol kinds (helix-flavored).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Class,
    Struct,
    Enum,
    Union,
    Interface,
    Trait,
    Impl,
    Function,
    Method,
    Constructor,
    Field,
    Constant,
    Static,
    Variable,
    TypeAlias,
    Namespace,
    Module,
    Macro,
    EnumMember,
    Operator,
}

impl SymbolKind {
    pub const ALL: [SymbolKind; 20] = [
        SymbolKind::Class,
        SymbolKind::Struct,
        SymbolKind::Enum,
        SymbolKind::Union,
        SymbolKind::Interface,
        SymbolKind::Trait,
        SymbolKind::Impl,
        SymbolKind::Function,
        SymbolKind::Method,
        SymbolKind::Constructor,
        SymbolKind::Field,
        SymbolKind::Constant,
        SymbolKind::Static,
        SymbolKind::Variable,
        SymbolKind::TypeAlias,
        SymbolKind::Namespace,
        SymbolKind::Module,
        SymbolKind::Macro,
        SymbolKind::EnumMember,
        SymbolKind::Operator,
    ];

    pub fn name(self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Union => "union",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Constructor => "constructor",
            SymbolKind::Field => "field",
            SymbolKind::Constant => "constant",
            SymbolKind::Static => "static",
            SymbolKind::Variable => "variable",
            SymbolKind::TypeAlias => "type-alias",
            SymbolKind::Namespace => "namespace",
            SymbolKind::Module => "module",
            SymbolKind::Macro => "macro",
            SymbolKind::EnumMember => "enum-member",
            SymbolKind::Operator => "operator",
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for SymbolKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_ascii_lowercase().replace('_', "-");
        Ok(match norm.as_str() {
            "class" => SymbolKind::Class,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "union" => SymbolKind::Union,
            "interface" => SymbolKind::Interface,
            "trait" => SymbolKind::Trait,
            "impl" | "implementation" => SymbolKind::Impl,
            "function" | "func" | "fn" => SymbolKind::Function,
            "method" | "member-function" => SymbolKind::Method,
            "constructor" | "ctor" => SymbolKind::Constructor,
            "field" | "member" | "member-variable" => SymbolKind::Field,
            "constant" | "const" => SymbolKind::Constant,
            "static" => SymbolKind::Static,
            "variable" | "var" => SymbolKind::Variable,
            "type-alias" | "typedef" | "type" | "alias" => SymbolKind::TypeAlias,
            "namespace" | "package" => SymbolKind::Namespace,
            "module" | "mod" => SymbolKind::Module,
            "macro" | "macros" => SymbolKind::Macro,
            "enum-member" | "enum-variant" | "variant" => SymbolKind::EnumMember,
            "operator" => SymbolKind::Operator,
            other => anyhow::bail!("unknown symbol kind `{other}`"),
        })
    }
}

/// One symbol occurrence, produced by the indexer or the single-file parser.
#[derive(Clone, Debug)]
pub struct SymbolRecord {
    pub name: String,
    pub kind: SymbolKind,
    /// Qualified enclosing scope, e.g. `ns::Outer` (None at file scope).
    pub container: Option<String>,
    /// 1-based start line.
    pub start_line: u32,
    /// 0-based start column (bytes).
    pub start_col: u32,
    /// 1-based end line (inclusive).
    pub end_line: u32,
    pub end_col: u32,
    /// One-line signature, e.g. `void ring_buffer::push(T value)`.
    pub signature: Option<String>,
}

/// Metadata about an indexed file.
#[derive(Clone, Debug)]
pub struct FileRecord {
    /// Path relative to the repo root, '/'-separated.
    pub path: String,
    pub lang: Lang,
    /// blake3 hash of the file contents.
    pub hash: String,
    pub size: u64,
}

/// A symbol search hit.
#[derive(Clone, Debug)]
pub struct SymbolHit {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    pub path: String,
    pub lang: Lang,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub signature: Option<String>,
}
