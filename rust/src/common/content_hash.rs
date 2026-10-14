//! SHA-256 content hashing (mirrors `symhow::common::ContentHash`).
//!
//! The C++ implementation hand-rolls SHA-256 (FIPS 180-4). The Rust port uses
//! the `sha2` crate, which produces byte-identical digests.

use std::io::Read;

use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// SHA-256 of the exact bytes, lowercase hex. No newline/whitespace
/// normalization.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// Streams a regular file; missing/unreadable/non-regular paths return `None`.
pub fn file_content_hash(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16384];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(hex(&hasher.finalize()))
}