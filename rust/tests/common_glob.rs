//! Port of test/unit/test_glob.cpp.

use symhow::common::glob::matches;

#[test]
fn matches_literal_strings_exactly() {
    assert!(matches("foo.cpp", "foo.cpp"));
    assert!(!matches("foo.cpp", "foo.cc"));
}

#[test]
fn star_matches_any_run_excluding_slash() {
    assert!(matches("*.cpp", "foo.cpp"));
    assert!(matches("src/*.cpp", "src/foo.cpp"));
    assert!(!matches("src/*.cpp", "src/nested/foo.cpp"));
    assert!(matches("*", ""));
}

#[test]
fn globstar_matches_any_run_including_slash() {
    assert!(matches("src/**/*.cpp", "src/a/b/c/foo.cpp"));
    assert!(matches("src/**/*.cpp", "src/foo.cpp"));
    assert!(matches(
        "**/*.cu",
        "/home/aaramoon/workspace/oss/cucs/test/memory/test_gpu_kernels.cu"
    ));
    assert!(!matches(
        "**/*.cu",
        "/home/aaramoon/workspace/oss/cucs/test/memory/test_gpu_kernels.cpp"
    ));
}

#[test]
fn question_matches_exactly_one_char() {
    assert!(matches("foo?.cpp", "food.cpp"));
    assert!(!matches("foo?.cpp", "foo.cpp"));
    assert!(!matches("foo?.cpp", "fooxy.cpp"));
}

#[test]
fn char_classes() {
    assert!(matches("foo[0-9].cpp", "foo3.cpp"));
    assert!(!matches("foo[0-9].cpp", "fooa.cpp"));
    assert!(matches("foo[!0-9].cpp", "fooa.cpp"));
    assert!(!matches("foo[!0-9].cpp", "foo3.cpp"));
    assert!(matches("foo[abc].cpp", "fooa.cpp"));
}

#[test]
fn real_cucs_benchmark_src_exclude_case() {
    assert!(matches(
        "**/_deps/**",
        "/home/aaramoon/workspace/oss/cucs/build/release/_deps/benchmark-src/src/benchmark.cc"
    ));
    assert!(!matches(
        "**/_deps/**",
        "/home/aaramoon/workspace/oss/cucs/src/memory/topology_discovery.cpp"
    ));
}