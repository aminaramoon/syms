//! Port of test/unit/test_arg_sanitizer.cpp.

use symhow::index::arg_sanitizer::{sanitize, SanitizerOptions};
use symhow::index::compile_db::{apply_remaps, parse_remap_arg, CompileEntry};
use symhow::index::cuda_support::CudaMode;

fn make_entry(file: &str, args_with_argv0: Vec<&str>, directory: &str, argv0: &str) -> CompileEntry {
    let mut args: Vec<String> = Vec::with_capacity(args_with_argv0.len() + 1);
    args.push(argv0.to_string());
    for a in args_with_argv0 {
        args.push(a.to_string());
    }
    CompileEntry {
        directory: directory.to_string(),
        file: file.to_string(),
        output: String::new(),
        args,
    }
}

/// Returns the portion of a fully-sanitized argv that precedes the step-5
/// injection block (which always starts with "-working-directory=...").
fn core_args(args: &[String]) -> Vec<String> {
    match args.iter().position(|t| t.starts_with("-working-directory=")) {
        Some(i) => args[..i].to_vec(),
        None => args.to_vec(),
    }
}

struct Case {
    name: &'static str,
    file: &'static str,
    input: Vec<&'static str>,    // argv, excluding argv0
    expected: Vec<&'static str>, // expected kept tokens (coreArgs)
    argv0: &'static str,
}

impl Case {
    fn new(
        name: &'static str,
        file: &'static str,
        input: Vec<&'static str>,
        expected: Vec<&'static str>,
    ) -> Self {
        Case {
            name,
            file,
            input,
            expected,
            argv0: "/usr/bin/c++",
        }
    }

    fn with_argv0(mut self, argv0: &'static str) -> Self {
        self.argv0 = argv0;
        self
    }
}

fn run_cases(cases: Vec<Case>, cuda_mode: CudaMode) {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(), // avoid depending on the default resource dir
        cuda_mode,
        ..Default::default()
    };

    for c in &cases {
        let entry = make_entry(c.file, c.input.clone(), "/work", c.argv0);
        let result = sanitize(&entry, &options);
        let core = core_args(&result.args);
        let actual: Vec<&str> = core.iter().map(|s| s.as_str()).collect();
        assert_eq!(actual, c.expected, "case: {}", c.name);
    }
}

#[test]
fn universal_drop_list() {
    let cases = vec![
        Case::new("-c dropped", "foo.cpp", vec!["-DKEEP", "-c", "-O2"], vec!["-DKEEP", "-O2"]),
        Case::new("-o standalone drops flag+value", "foo.cpp", vec!["-DKEEP", "-o", "foo.o", "-O2"], vec!["-DKEEP", "-O2"]),
        Case::new("-o attached dropped", "foo.cpp", vec!["-DKEEP", "-ofoo.o", "-O2"], vec!["-DKEEP", "-O2"]),
        Case::new("-MD dropped (no-arg)", "foo.cpp", vec!["-DKEEP", "-MD", "-O2"], vec!["-DKEEP", "-O2"]),
        Case::new("-MMD dropped (no-arg)", "foo.cpp", vec!["-MMD"], vec![]),
        Case::new("-MP dropped (no-arg)", "foo.cpp", vec!["-MP"], vec![]),
        Case::new("-MG dropped (no-arg)", "foo.cpp", vec!["-MG"], vec![]),
        Case::new("-M dropped (no-arg)", "foo.cpp", vec!["-M"], vec![]),
        Case::new("-MM dropped (no-arg)", "foo.cpp", vec!["-MM"], vec![]),
        Case::new("-MF drops flag+value", "foo.cpp", vec!["-DKEEP", "-MF", "foo.d", "-O2"], vec!["-DKEEP", "-O2"]),
        Case::new("-MT drops flag+value", "foo.cpp", vec!["-MT", "foo.o"], vec![]),
        Case::new("-MQ drops flag+value", "foo.cpp", vec!["-MQ", "foo.o"], vec![]),
        Case::new("-fmodules-ts dropped", "foo.cpp", vec!["-fmodules-ts"], vec![]),
        Case::new("-fmodule-mapper= dropped (prefix)", "foo.cpp", vec!["-fmodule-mapper=foo.modmap"], vec![]),
        Case::new("-fdeps-format= dropped (prefix)", "foo.cpp", vec!["-fdeps-format=p1689r5"], vec![]),
        Case::new("-fdeps-file= dropped (prefix)", "foo.cpp", vec!["-fdeps-file=foo.ddi"], vec![]),
        Case::new("-fdeps-target= dropped (prefix)", "foo.cpp", vec!["-fdeps-target=foo.o"], vec![]),
        Case::new("-Werror dropped", "foo.cpp", vec!["-Werror"], vec![]),
        Case::new("-Werror= dropped (prefix)", "foo.cpp", vec!["-Werror=unused"], vec![]),
        Case::new("-pedantic-errors dropped", "foo.cpp", vec!["-pedantic-errors"], vec![]),
        Case::new("-fdiagnostics-color dropped (prefix)", "foo.cpp", vec!["-fdiagnostics-color=always"], vec![]),
        Case::new("-l* dropped (link-only)", "foo.cpp", vec!["-lfoo"], vec![]),
        Case::new("-L* dropped (link-only)", "foo.cpp", vec!["-L/usr/lib"], vec![]),
        Case::new("-Wl,* dropped (link-only)", "foo.cpp", vec!["-Wl,--as-needed"], vec![]),
        Case::new("-shared dropped (link-only)", "foo.cpp", vec!["-shared"], vec![]),
        Case::new("-rdynamic dropped (link-only)", "foo.cpp", vec!["-rdynamic"], vec![]),
        Case::new("-pie dropped (link-only)", "foo.cpp", vec!["-pie"], vec![]),
        Case::new("-fuse-ld= dropped (link-only, prefix)", "foo.cpp", vec!["-fuse-ld=lld"], vec![]),
        Case::new("-resource-dir (separate) dropped, consumes value", "foo.cpp", vec!["-resource-dir", "/some/dir"], vec![]),
        Case::new("-resource-dir= (joined) dropped", "foo.cpp", vec!["-resource-dir=/some/dir"], vec![]),
        Case::new("--resource-dir= (joined) dropped", "foo.cpp", vec!["--resource-dir=/some/dir"], vec![]),
        Case::new(
            "unrecognized flags preserved verbatim",
            "foo.cpp",
            vec!["-DFOO=1", "-Ifoo", "-isystem", "/sys", "-O3", "-std=c++20", "-march=native", "-fPIC", "-pipe", "-pthread", "-Wall"],
            vec!["-DFOO=1", "-Ifoo", "-isystem", "/sys", "-O3", "-std=c++20", "-march=native", "-fPIC", "-pipe", "-pthread", "-Wall"],
        ),
    ];
    run_cases(cases, CudaMode::Auto);
}

#[test]
fn nvcc_specific_rules_triggered_by_argv0_containing_nvcc() {
    let nvcc = "/opt/cuda/bin/nvcc";
    let host_only_tail = vec!["-x", "c++"];
    let cases = vec![
        Case::new("-forward-unknown-to-host-compiler dropped", "foo.cu", vec!["-forward-unknown-to-host-compiler"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--generate-code= (joined) dropped", "foo.cu", vec!["--generate-code=arch=compute_75,code=[sm_75]"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-gencode arch=... (separate) dropped", "foo.cu", vec!["-gencode", "arch=compute_80,code=sm_80"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-gencode= (joined) dropped", "foo.cu", vec!["-gencode=arch=compute_80,code=sm_80"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--gpu-architecture= dropped", "foo.cu", vec!["--gpu-architecture=compute_75"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-Xcompiler=<flag> unwraps to bare flag", "foo.cu", vec!["-Xcompiler=-fPIE"], vec!["-fPIE", "-x", "c++"]).with_argv0(nvcc),
        Case::new("-Xcompiler <flag> (separate) unwraps to bare flag", "foo.cu", vec!["-Xcompiler", "-pthread"], vec!["-pthread", "-x", "c++"]).with_argv0(nvcc),
        Case::new("-Xcudafe <arg> dropped (with value)", "foo.cu", vec!["-Xcudafe", "--diag_suppress=x"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-Xptxas <arg> dropped (with value)", "foo.cu", vec!["-Xptxas", "-v"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-Xfatbin <arg> dropped (with value)", "foo.cu", vec!["-Xfatbin", "-compress-all"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-Xlinker <arg> dropped (with value)", "foo.cu", vec!["-Xlinker", "-rpath"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-ccbin <arg> dropped (with value)", "foo.cu", vec!["-ccbin", "g++"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--compiler-options <arg> dropped (with value)", "foo.cu", vec!["--compiler-options", "-fPIC"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-rdc= dropped (prefix)", "foo.cu", vec!["-rdc=true"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--relocatable-device-code= dropped (prefix)", "foo.cu", vec!["--relocatable-device-code=true"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-lineinfo dropped", "foo.cu", vec!["-lineinfo"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-G dropped", "foo.cu", vec!["-G"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--default-stream <x> dropped (with value)", "foo.cu", vec!["--default-stream", "per-thread"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--expt-relaxed-constexpr dropped", "foo.cu", vec!["--expt-relaxed-constexpr"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--expt-extended-lambda dropped", "foo.cu", vec!["--expt-extended-lambda"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("--extended-lambda dropped", "foo.cu", vec!["--extended-lambda"], host_only_tail.clone()).with_argv0(nvcc),
        Case::new("-x cu translated to -x cuda", "foo.cu", vec!["-x", "cu"], vec!["-x", "cuda", "-x", "c++"]).with_argv0(nvcc),
    ];
    run_cases(cases, CudaMode::HostOnly);
}

#[test]
fn nvcc_rules_also_trigger_for_a_cu_file_even_with_a_non_nvcc_argv0() {
    let cases = vec![Case::new(
        "-forward-unknown-to-host-compiler dropped for .cu file regardless of argv0",
        "kernel.cu",
        vec!["-forward-unknown-to-host-compiler", "-DKEEP"],
        vec!["-DKEEP", "-x", "c++"],
    )];
    run_cases(cases, CudaMode::HostOnly);
}

#[test]
fn universal_drop_list_still_applies_inside_an_nvcc_invocation() {
    let cases = vec![Case::new(
        "-Werror dropped even though nvcc rules are active",
        "foo.cu",
        vec!["-Werror", "-DKEEP"],
        vec!["-DKEEP", "-x", "c++"],
    )
    .with_argv0("/opt/cuda/bin/nvcc")];
    run_cases(cases, CudaMode::HostOnly);
}

#[test]
fn user_drop_arg_glob_patterns() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        drop_arg_patterns: vec!["-DSECRET*".to_string(), "-W*".to_string()],
        ..Default::default()
    };

    let entry = make_entry(
        "foo.cpp",
        vec!["-DKEEP=1", "-DSECRET_TOKEN=abc", "-Wall", "-O2"],
        "/work",
        "/usr/bin/c++",
    );
    let result = sanitize(&entry, &options);
    assert_eq!(
        core_args(&result.args),
        vec!["-DKEEP=1".to_string(), "-O2".to_string()]
    );
}

#[test]
fn injections_are_present_and_extra_arg_values_are_appended_last() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        gcc_toolchain: "/opt/toolchain".to_string(),
        extra_args: vec!["-DEXTRA1".to_string(), "-DEXTRA2".to_string()],
        ..Default::default()
    };

    let entry = make_entry("foo.cpp", vec!["-DKEEP=1"], "/some/work/dir", "/usr/bin/c++");
    let result = sanitize(&entry, &options);

    assert_eq!(result.args.len(), 10);
    let args = &result.args;

    // Kept token first, then the fixed injection block, in the documented order.
    assert_eq!(args[0], "-DKEEP=1");
    assert_eq!(args[1], "-working-directory=/some/work/dir");
    assert_eq!(args[2], "-resource-dir=/opt/resource-dir");
    assert_eq!(args[3], "-fsyntax-only");
    assert_eq!(args[4], "-w");
    assert_eq!(args[5], "-ferror-limit=0");
    assert_eq!(args[6], "-std=c++17"); // no -std= survived rewriting -> fallback injected
    assert_eq!(args[7], "--gcc-toolchain=/opt/toolchain");
    assert_eq!(args[8], "-DEXTRA1");
    assert_eq!(args[9], "-DEXTRA2");
}

#[test]
fn std_fallback_is_not_injected_when_a_std_token_survived_rewriting() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.cpp", vec!["-std=c++20"], "/work", "/usr/bin/c++");
    let result = sanitize(&entry, &options);

    let std_count = result.args.iter().filter(|t| t.starts_with("-std=")).count();
    assert_eq!(std_count, 1);
    assert!(result.args.iter().any(|t| t == "-std=c++20"));
}

#[test]
fn std_cli_value_overrides_the_cpp17_default_fallback() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        std_fallback: "c++23".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.cpp", vec!["-DKEEP"], "/work", "/usr/bin/c++");
    let result = sanitize(&entry, &options);
    assert!(result.args.iter().any(|t| t == "-std=c++23"));
}

#[test]
fn c_file_with_no_std_gets_the_c_fallback_gnu17_not_cpp17() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.c", vec!["-DKEEP"], "/work", "/usr/bin/cc");
    let result = sanitize(&entry, &options);
    assert!(result.args.iter().any(|t| t == "-std=gnu17"));
    assert!(!result.args.iter().any(|t| t == "-std=c++17"));
}

#[test]
fn c_file_with_an_explicit_std_c11_is_left_untouched() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.c", vec!["-std=c11"], "/work", "/usr/bin/cc");
    let result = sanitize(&entry, &options);
    let std_count = result.args.iter().filter(|t| t.starts_with("-std=")).count();
    assert_eq!(std_count, 1);
    assert!(result.args.iter().any(|t| t == "-std=c11"));
}

#[test]
fn cpp_file_with_no_std_still_gets_the_cpp_fallback_cpp17() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.cpp", vec!["-DKEEP"], "/work", "/usr/bin/c++");
    let result = sanitize(&entry, &options);
    assert!(result.args.iter().any(|t| t == "-std=c++17"));
}

#[test]
fn dash_x_c_on_a_cpp_named_file_gets_the_c_fallback_not_cpp() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.cpp", vec!["-x", "c", "-DKEEP"], "/work", "/usr/bin/c++");
    let result = sanitize(&entry, &options);
    assert!(result.args.iter().any(|t| t == "-std=gnu17"));
    assert!(!result.args.iter().any(|t| t == "-std=c++17"));
}

#[test]
fn c_std_cli_value_overrides_the_gnu17_default_c_fallback() {
    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        c_std_fallback: "c99".to_string(),
        ..Default::default()
    };

    let entry = make_entry("foo.c", vec!["-DKEEP"], "/work", "/usr/bin/cc");
    let result = sanitize(&entry, &options);
    assert!(result.args.iter().any(|t| t == "-std=c99"));
}

// --- Remap substring replacement (step 1) -----------------------------------

#[test]
fn parse_remap_arg_splits_old_new_on_the_first_equals() {
    let (old_str, new_str) = parse_remap_arg("/mnt/workspace=/home/aaramoon/workspace").unwrap();
    assert_eq!(old_str, "/mnt/workspace");
    assert_eq!(new_str, "/home/aaramoon/workspace");

    assert!(parse_remap_arg("no-equals-sign").is_err());
}

#[test]
fn apply_remaps_literal_substring_replacement_not_regex() {
    let remaps = vec![(
        "/mnt/workspace".to_string(),
        "/home/aaramoon/workspace".to_string(),
    )];
    assert_eq!(
        apply_remaps("/mnt/workspace/oss/cucs/include", &remaps),
        "/home/aaramoon/workspace/oss/cucs/include"
    );
    // Applies inside flag values too.
    assert_eq!(
        apply_remaps("-I/mnt/workspace/oss/cucs/include", &remaps),
        "-I/home/aaramoon/workspace/oss/cucs/include"
    );
    assert_eq!(apply_remaps("no match here", &remaps), "no match here");
}

#[test]
fn apply_remaps_multiple_remap_pairs_chain_in_order() {
    let remaps = vec![
        (
            "/mnt/workspace".to_string(),
            "/home/aaramoon/workspace".to_string(),
        ),
        (
            "/home/aaramoon/workspace/oss".to_string(),
            "/srv/oss".to_string(),
        ),
    ];
    assert_eq!(
        apply_remaps("/mnt/workspace/oss/cucs", &remaps),
        "/srv/oss/cucs"
    );
}

#[test]
fn remap_applies_to_directory_file_and_every_arg_token_via_the_full_pipeline() {
    let mut entry = CompileEntry {
        directory: "/mnt/workspace/oss/cucs/build/release".to_string(),
        file: "/mnt/workspace/oss/cucs/src/foo.cpp".to_string(),
        output: String::new(),
        args: vec![
            "/mnt/workspace/oss/cucs/.pixi/envs/default/bin/x86_64-conda-linux-gnu-c++".to_string(),
            "-I/mnt/workspace/oss/cucs/include".to_string(),
            "-isystem".to_string(),
            "/mnt/workspace/oss/cucs/.pixi/envs/default/include".to_string(),
        ],
    };

    let remaps = vec![(
        "/mnt/workspace".to_string(),
        "/home/aaramoon/workspace".to_string(),
    )];
    entry.directory = apply_remaps(&entry.directory, &remaps);
    entry.file = apply_remaps(&entry.file, &remaps);
    for a in &mut entry.args {
        *a = apply_remaps(a, &remaps);
    }

    let options = SanitizerOptions {
        resource_dir: "/opt/resource-dir".to_string(),
        ..Default::default()
    };
    let result = sanitize(&entry, &options);

    for tok in &result.args {
        assert!(!tok.contains("/mnt/workspace"), "token: {}", tok);
    }
    assert!(result
        .args
        .iter()
        .any(|t| t == "-I/home/aaramoon/workspace/oss/cucs/include"));
}