//! Error type for the symhow library.
//!
//! The C++ code throws `std::runtime_error` with a human-readable message
//! everywhere; the faithful Rust equivalent is a single message-carrying
//! error type rather than a rich enum of variants. Call sites build it with
//! `Error::new(...)` and propagate it with `?`.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;