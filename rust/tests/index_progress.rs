//! Port of test/unit/test_progress.cpp.

use symhow::index::progress::{
    format_counter, format_duration_hms, format_progress_line, format_running_summary,
    format_tu_seconds, relative_to_root,
};

#[test]
fn format_counter_right_aligns_to_totals_digit_width() {
    assert_eq!(format_counter(12, 975), "[ 12/975]");
    assert_eq!(format_counter(1, 975), "[  1/975]");
    assert_eq!(format_counter(975, 975), "[975/975]");
    assert_eq!(format_counter(1, 9), "[1/9]");
    assert_eq!(format_counter(1, 10), "[ 1/10]");
    assert_eq!(format_counter(5, 1), "[5/1]"); // width from total's OWN digit count, never index's
}

#[test]
fn format_tu_seconds_always_shows_one_decimal() {
    assert_eq!(format_tu_seconds(1.3), "1.3s");
    assert_eq!(format_tu_seconds(0.04), "0.0s");
    assert_eq!(format_tu_seconds(4.1), "4.1s");
    assert_eq!(format_tu_seconds(-1.0), "0.0s"); // clamped, never negative
}

#[test]
fn format_duration_hms_scales_units_and_zero_pads() {
    assert_eq!(format_duration_hms(0.0), "0s");
    assert_eq!(format_duration_hms(41.0), "41s");
    assert_eq!(format_duration_hms(59.6), "1m00s"); // rounds to nearest second
    assert_eq!(format_duration_hms(9.0 * 60.0 + 41.0), "9m41s");
    assert_eq!(format_duration_hms(60.0 * 60.0 + 8.0 * 60.0 + 20.0), "1h08m20s");
    assert_eq!(format_duration_hms(-5.0), "0s"); // clamped
}

#[test]
fn format_progress_line_assembles_counter_time_status_path_in_order() {
    let line = format_progress_line(12, 975, 1.3, "ok", "src/foo/bar.cpp");
    assert_eq!(line, "[ 12/975] 1.3s  ok  src/foo/bar.cpp");
}

#[test]
fn format_progress_line_carries_a_cuda_rung_suffix_through_unchanged() {
    let line = format_progress_line(13, 975, 4.1, "ok (cuda rung 1)", "src/op/x.cu");
    assert_eq!(line, "[ 13/975] 4.1s  ok (cuda rung 1)  src/op/x.cu");
}

#[test]
fn format_running_summary_includes_an_eta_while_in_progress_omits_it_once_done() {
    let mid = format_running_summary(120, 975, 12.3, 9.0 * 60.0 + 41.0, 42.0 * 60.0);
    assert_eq!(
        mid,
        "symhow index: 120/975 done, 12.3 TU/min, elapsed 9m41s, eta ~42m00s"
    );

    let done = format_running_summary(975, 975, 12.3, 9.0 * 60.0 + 41.0, 0.0);
    assert_eq!(
        done,
        "symhow index: 975/975 done, 12.3 TU/min, elapsed 9m41s"
    );
    assert!(!done.contains("eta"));
}

#[test]
fn relative_to_root_strips_the_repo_root_and_normalizes_a_bare_match_to_dot() {
    assert_eq!(relative_to_root("/repo/src/foo.cpp", "/repo"), "src/foo.cpp");
    assert_eq!(
        relative_to_root("/repo/src/foo.cpp", "/repo/"),
        "src/foo.cpp"
    ); // trailing slash tolerated
    assert_eq!(relative_to_root("/repo", "/repo"), ".");
    assert_eq!(
        relative_to_root("/other/src/foo.cpp", "/repo"),
        "/other/src/foo.cpp"
    ); // outside root: unchanged
    assert_eq!(
        relative_to_root("/repo/src/foo.cpp", ""),
        "/repo/src/foo.cpp"
    ); // no known root: unchanged
    assert_eq!(
        relative_to_root("/repofoo/x.cpp", "/repo"),
        "/repofoo/x.cpp"
    ); // no false-positive prefix match
}