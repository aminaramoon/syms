//! Port of test/unit/test_process.cpp.

use symhow::common::process::run_str;

#[test]
fn runs_echo_and_captures_stdout() {
    let r = run_str(["/bin/echo", "hi"], None);
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.out, "hi\n");
}

#[test]
fn nonexistent_binary_fails_to_spawn_without_panicking() {
    let r = run_str(["symhow-this-binary-does-not-exist-anywhere-1234"], None);
    assert_eq!(r.exit_code, -1);
    assert!(!r.err.is_empty());
}

#[test]
fn relays_nonzero_exit_code() {
    let r = run_str(["/bin/sh", "-c", "exit 3"], None);
    assert_eq!(r.exit_code, 3);
}

#[test]
fn captures_stderr_separately_from_stdout() {
    let r = run_str(["/bin/sh", "-c", "echo out; echo err 1>&2"], None);
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.out, "out\n");
    assert_eq!(r.err, "err\n");
}

#[test]
fn draining_does_not_deadlock_on_large_stderr() {
    let r = run_str(
        ["/bin/sh", "-c", "head -c 200000 /dev/zero | tr '\\0' x >&2; echo done"],
        None,
    );
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.out, "done\n");
    assert_eq!(r.err.len(), 200000);
}

#[test]
fn resolves_argv0_via_path() {
    let r = run_str(["true"], None);
    assert_eq!(r.exit_code, 0);
}

#[test]
fn cwd_changes_the_child_working_directory() {
    let r = run_str(["/bin/sh", "-c", "pwd"], Some("/tmp"));
    assert_eq!(r.exit_code, 0);
    assert!(!r.out.is_empty());
}