//! Port of test/unit/test_shell_split.cpp.

use symhow::common::shell::split;

#[test]
fn plain_space_separated_tokens() {
    assert_eq!(
        split("gcc -c foo.cpp -o foo.o"),
        vec!["gcc", "-c", "foo.cpp", "-o", "foo.o"]
    );
}

#[test]
fn collapses_repeated_whitespace_and_trims() {
    assert_eq!(split("  gcc   -c   foo.cpp  "), vec!["gcc", "-c", "foo.cpp"]);
}

#[test]
fn single_quoted_tokens_literal() {
    assert_eq!(split(r#"gcc -DFOO='bar baz' -c"#), vec!["gcc", "-DFOO=bar baz", "-c"]);
}

#[test]
fn single_quotes_fully_literal_no_escapes() {
    assert_eq!(split(r#"'a\b'"#), vec![r#"a\b"#]);
}

#[test]
fn double_quoted_tokens() {
    assert_eq!(split(r#"gcc -DFOO="bar baz" -c"#), vec!["gcc", "-DFOO=bar baz", "-c"]);
}

#[test]
fn backslash_escapes_inside_double_quotes() {
    assert_eq!(split(r#""a \" b \\ c""#), vec![r#"a " b \ c"#]);
}

#[test]
fn backslash_escapes_outside_quotes() {
    assert_eq!(split(r#"foo\ bar baz"#), vec!["foo bar", "baz"]);
}

#[test]
fn concatenates_adjacent_quoted_unquoted_segments() {
    assert_eq!(split(r#"foo"bar baz"qux"#), vec!["foobar bazqux"]);
}

#[test]
fn real_cucs_generate_code_token() {
    let tokens = split(r#""--generate-code=arch=compute_75,code=[sm_75]""#);
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0], "--generate-code=arch=compute_75,code=[sm_75]");
}

#[test]
fn full_nvcc_generate_code_sequence() {
    let tokens = split(
        r#""--generate-code=arch=compute_75,code=[sm_75]" "--generate-code=arch=compute_80,code=[sm_80]" -Xcompiler=-fPIE -Xcompiler -pthread -x cu"#,
    );
    assert_eq!(
        tokens,
        vec![
            "--generate-code=arch=compute_75,code=[sm_75]",
            "--generate-code=arch=compute_80,code=[sm_80]",
            "-Xcompiler=-fPIE",
            "-Xcompiler",
            "-pthread",
            "-x",
            "cu",
        ]
    );
}

#[test]
fn empty_or_whitespace_only_returns_empty() {
    assert!(split("").is_empty());
    assert!(split("   ").is_empty());
}