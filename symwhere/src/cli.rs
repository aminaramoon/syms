//! Command-line interface definition for `symwhere`.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Language selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LangArg {
    /// C
    C,
    /// C++
    Cpp,
    /// CUDA (.cu/.cuh, parsed with the C++ grammar)
    Cuda,
    /// Rust
    Rust,
}

/// Symbol kind filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum KindArg {
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

#[derive(Parser)]
#[command(
    name = "symwhere",
    version,
    about = "Fast, fuzzy symbol index for C/C++/CUDA/Rust repos, powered by tree-sitter",
    after_help = "Examples:\n  symwhere index .\n  symwhere update . --git-diff\n  symwhere find \"buffer\" --type class,struct -l cpp\n  symwhere symbols src/foo.cpp --within 120-180"
)]
pub struct Cli {
    /// Path to the symbol database. Defaults to <REPO>/.symwhere/index.db
    /// (or <repo>/index.db when --db is a directory).
    #[arg(long, short = 'd', global = true)]
    pub db: Option<PathBuf>,

    /// Number of worker threads (default: number of available cores).
    #[arg(long, short = 'j', global = true)]
    pub jobs: Option<usize>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Build a full symbol index for a repository.
    Index {
        /// Repository root (default: current directory).
        path: Option<PathBuf>,

        /// Only index these languages (repeatable: --lang cpp --lang rust).
        #[arg(long, short = 'l')]
        lang: Vec<LangArg>,

        /// Exclude files/directories matching these globs (repeatable).
        #[arg(long, short = 'e')]
        exclude: Vec<String>,

        /// Only include files/directories matching these globs (repeatable).
        #[arg(long, short = 'i')]
        include: Vec<String>,

        /// Rebuild the database from scratch even if it exists.
        #[arg(long, short = 'f')]
        force: bool,
    },

    /// Incrementally update an existing index.
    Update {
        /// Repository root (default: current directory).
        path: Option<PathBuf>,

        /// Use `git diff` to detect changed files instead of scanning.
        /// Optional ref: diff against it (e.g. HEAD~1). With no ref, diffs
        /// against the commit stored at last index time.
        #[arg(long, num_args = 0..=1, default_missing_value = "LAST")]
        git_diff: Option<String>,

        /// Print what would change without writing to the database.
        #[arg(long)]
        dry_run: bool,

        #[arg(long, short = 'l')]
        lang: Vec<LangArg>,
        #[arg(long, short = 'e')]
        exclude: Vec<String>,
        #[arg(long, short = 'i')]
        include: Vec<String>,
    },

    /// Fuzzy-search the index for symbols.
    Find {
        /// Fuzzy query strings (e.g. `buf` matches "ring_buffer").
        queries: Vec<String>,

        /// Restrict to symbol kinds (repeatable or comma-separated).
        #[arg(long, short = 't', value_delimiter = ',')]
        r#type: Vec<KindArg>,

        #[arg(long, short = 'l')]
        lang: Vec<LangArg>,
        #[arg(long, short = 'e')]
        exclude: Vec<String>,
        #[arg(long, short = 'i')]
        include: Vec<String>,

        /// Exact substring match instead of fuzzy.
        #[arg(long)]
        exact: bool,

        /// Maximum results.
        #[arg(long, short = 'n', default_value = "50")]
        limit: usize,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Include the definition end position: `line:col-endline:endcol`.
        #[arg(long)]
        spans: bool,
    },

    /// List symbols in a single file (no database needed).
    Symbols {
        /// File to parse.
        file: PathBuf,

        /// Restrict to symbol kinds.
        #[arg(long, short = 't', value_delimiter = ',')]
        r#type: Vec<KindArg>,

        /// Show only symbols that enclose this line, or a range `A-B`.
        #[arg(long, value_name = "LINE|RANGE")]
        within: Option<String>,

        /// Override language detection.
        #[arg(long, short = 'l')]
        lang: Option<LangArg>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Include the definition end position: `line:col-endline:endcol`.
        #[arg(long)]
        spans: bool,
    },

    /// List supported languages and symbol kinds.
    Langs,
}

impl From<LangArg> for crate::types::Lang {
    fn from(a: LangArg) -> Self {
        match a {
            LangArg::C => crate::types::Lang::C,
            LangArg::Cpp => crate::types::Lang::Cpp,
            LangArg::Cuda => crate::types::Lang::Cuda,
            LangArg::Rust => crate::types::Lang::Rust,
        }
    }
}
