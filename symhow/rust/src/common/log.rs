//! Minimal logging facade (mirrors `symhow::common::logInfo`): a single
//! function that writes one line to stderr, prefixed `[symhow]`.

pub fn log_info(message: &str) {
    eprintln!("[symhow] {}", message);
}