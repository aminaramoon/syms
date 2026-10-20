//! Backend-neutral fact-extraction model (mirrors `symhow::model`).

pub mod enums;
pub mod fact_sink;
pub mod facts;

pub use enums::*;
pub use fact_sink::{BatchSink, FactSink, IndexBackend, IndexSummary};
pub use facts::*;