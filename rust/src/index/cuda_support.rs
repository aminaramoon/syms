//! CUDA-flag synthesis (step 4 of the argument sanitizing pipeline). Mirrors
//! `symhow::index::cuda`.

use crate::common::{Error, Result};

/// One "rung" of CUDA-handling strategy that clang can be asked to try when
/// parsing a .cu file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CudaMode {
    Auto,
    Full,
    NoWrapper,
    HostOnly,
    Skip,
}

/// Parses a --cuda-mode CLI value ("auto", "full", "nowrapper", "host-only",
/// "skip"). Errors on anything else.
pub fn parse_cuda_mode(value: &str) -> Result<CudaMode> {
    match value {
        "auto" => Ok(CudaMode::Auto),
        "full" => Ok(CudaMode::Full),
        "nowrapper" => Ok(CudaMode::NoWrapper),
        "host-only" => Ok(CudaMode::HostOnly),
        "skip" => Ok(CudaMode::Skip),
        _ => Err(Error::new(format!(
            "invalid --cuda-mode value '{}' (expected auto, full, nowrapper, host-only, or skip)",
            value
        ))),
    }
}

/// Returns a human-readable name for `mode` (the inverse of `parse_cuda_mode`).
pub fn cuda_mode_name(mode: CudaMode) -> String {
    match mode {
        CudaMode::Auto => "auto".to_string(),
        CudaMode::Full => "full".to_string(),
        CudaMode::NoWrapper => "nowrapper".to_string(),
        CudaMode::HostOnly => "host-only".to_string(),
        CudaMode::Skip => "skip".to_string(),
    }
}

/// Resolves CudaMode::Auto to a concrete mode. Phase 2: Auto simply resolves
/// to Full.
pub fn resolve_auto_mode(mode: CudaMode) -> CudaMode {
    if mode == CudaMode::Auto {
        CudaMode::Full
    } else {
        mode
    }
}

/// Inputs to `synthesize()` that vary per translation unit / per CDB entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CudaSynthesisInput {
    /// Highest compute_N / sm_N number seen across the entry's
    /// --generate-code/-gencode/--gpu-architecture flags. Defaults to 90 if
    /// the entry had none at all.
    pub max_arch: i32,
    /// Resolved CUDA toolkit root (the directory containing bin/nvcc,
    /// include/, etc.), or empty if it couldn't be resolved.
    pub cuda_path: String,
}

impl Default for CudaSynthesisInput {
    fn default() -> Self {
        CudaSynthesisInput {
            max_arch: 90,
            cuda_path: String::new(),
        }
    }
}

impl CudaSynthesisInput {
    pub fn new(max_arch: i32, cuda_path: impl Into<String>) -> Self {
        CudaSynthesisInput {
            max_arch,
            cuda_path: cuda_path.into(),
        }
    }
}

/// Step 4: returns the CUDA-related flags to append to a .cu TU's argv for the
/// given (already-resolved, non-Auto) rung.
pub fn synthesize(mode: CudaMode, input: &CudaSynthesisInput) -> Vec<String> {
    match mode {
        CudaMode::Auto => synthesize(resolve_auto_mode(mode), input),

        CudaMode::Full => {
            let mut flags = vec!["-x".to_string(), "cuda".to_string()];
            if !input.cuda_path.is_empty() {
                flags.push(format!("--cuda-path={}", input.cuda_path));
            }
            flags.push(format!("--cuda-gpu-arch=sm_{}", input.max_arch));
            flags.push("--cuda-host-only".to_string());
            flags
        }

        CudaMode::NoWrapper => vec![
            "-x".to_string(),
            "cuda".to_string(),
            "-nocudainc".to_string(),
            "-nocudalib".to_string(),
            "--cuda-host-only".to_string(),
        ],

        CudaMode::HostOnly => vec!["-x".to_string(), "c++".to_string()],

        CudaMode::Skip => vec![],
    }
}