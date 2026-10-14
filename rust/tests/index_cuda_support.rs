//! Port of test/unit/test_cuda_support.cpp.

use symhow::index::cuda_support::{
    parse_cuda_mode, resolve_auto_mode, synthesize, CudaMode, CudaSynthesisInput,
};

fn representative_input() -> CudaSynthesisInput {
    CudaSynthesisInput::new(90, "/opt/cuda-toolkit")
}

#[test]
fn parse_cuda_mode_round_trips_all_valid_spellings() {
    assert_eq!(parse_cuda_mode("auto").unwrap(), CudaMode::Auto);
    assert_eq!(parse_cuda_mode("full").unwrap(), CudaMode::Full);
    assert_eq!(parse_cuda_mode("nowrapper").unwrap(), CudaMode::NoWrapper);
    assert_eq!(parse_cuda_mode("host-only").unwrap(), CudaMode::HostOnly);
    assert_eq!(parse_cuda_mode("skip").unwrap(), CudaMode::Skip);
    assert!(parse_cuda_mode("bogus").is_err());
}

#[test]
fn resolve_auto_mode_resolves_auto_to_full_for_phase_2() {
    assert_eq!(resolve_auto_mode(CudaMode::Auto), CudaMode::Full);
    assert_eq!(resolve_auto_mode(CudaMode::Full), CudaMode::Full);
    assert_eq!(resolve_auto_mode(CudaMode::NoWrapper), CudaMode::NoWrapper);
    assert_eq!(resolve_auto_mode(CudaMode::HostOnly), CudaMode::HostOnly);
    assert_eq!(resolve_auto_mode(CudaMode::Skip), CudaMode::Skip);
}

#[test]
fn synthesize_full_rung_emits_x_cuda_cuda_path_gpu_arch_host_only() {
    let flags = synthesize(CudaMode::Full, &representative_input());
    assert_eq!(
        flags,
        vec![
            "-x",
            "cuda",
            "--cuda-path=/opt/cuda-toolkit",
            "--cuda-gpu-arch=sm_90",
            "--cuda-host-only"
        ]
    );
}

#[test]
fn synthesize_full_rung_omits_cuda_path_when_unknown() {
    let mut input = representative_input();
    input.cuda_path.clear();
    let flags = synthesize(CudaMode::Full, &input);
    assert_eq!(
        flags,
        vec!["-x", "cuda", "--cuda-gpu-arch=sm_90", "--cuda-host-only"]
    );
}

#[test]
fn synthesize_nowrapper_rung_emits_only_x_cuda_nocudainc_nocudalib_host_only() {
    let flags = synthesize(CudaMode::NoWrapper, &representative_input());
    assert_eq!(
        flags,
        vec!["-x", "cuda", "-nocudainc", "-nocudalib", "--cuda-host-only"]
    );
}

#[test]
fn synthesize_hostonly_rung_emits_only_x_cxx() {
    let flags = synthesize(CudaMode::HostOnly, &representative_input());
    assert_eq!(flags, vec!["-x", "c++"]);
}

#[test]
fn synthesize_skip_rung_emits_no_flags_at_all() {
    let flags = synthesize(CudaMode::Skip, &representative_input());
    assert!(flags.is_empty());
}

#[test]
fn synthesize_auto_defers_to_full_phase_2_behavior() {
    let auto_flags = synthesize(CudaMode::Auto, &representative_input());
    let full_flags = synthesize(CudaMode::Full, &representative_input());
    assert_eq!(auto_flags, full_flags);
}