//! symhow — Rust backend.
//!
//! A faithful Rust migration of the symhow C++ CLI: index C++/CUDA code (via
//! libclang) into a Sourcetrail-schema SQLite database and query it. The
//! `.srctrldb` *write* path stays C++ (third_party/sourcetraildb, behind the
//! `ffi` C ABI shim); everything else is pure Rust.

pub mod cli;
pub mod common;
pub mod db;
pub mod index;
pub mod model;
pub mod notes;
pub mod out;
pub mod query;
pub mod srctrldb;
pub mod update;