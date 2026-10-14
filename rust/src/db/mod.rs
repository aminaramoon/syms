//! Database layer: read/write access to the Sourcetrail-schema SQLite format.

pub mod database;
pub mod enums;
pub mod meta;
pub mod name_decode;
pub mod writable_db;

pub use database::{ensure_indices, expected_index_names, missing_index_names, Database};
pub use writable_db::WritableDb;