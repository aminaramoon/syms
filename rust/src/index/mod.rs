//! Index layer (mirrors `symhow::index`). The libclang AST walker and the
//! SourcetrailDB writer bridge live here; the pure helpers in `clangutil`
//! have no libclang dependency and are also used by the notes layer.

pub mod arg_sanitizer;
pub mod ast_walker;
pub mod clangutil;
pub mod compile_db;
pub mod cuda_support;
pub mod file_registry;
pub mod index_driver;
pub mod name_builder;
pub mod progress;
pub mod provenance;
pub mod srctrldb_backend;
pub mod tu_parser;