//! Port of test/unit/test_fuzzy.cpp.

use symhow::query::{fuzzy_match, FuzzyHit};

fn score_for(hits: &[FuzzyHit], id: i64) -> i32 {
    for h in hits {
        if h.node_id == id {
            return h.score;
        }
    }
    panic!("candidate id {} did not match at all", id);
}

#[test]
fn candidate_that_never_contains_pattern_as_subsequence_does_not_match() {
    let candidates = vec![(1i64, "banana".to_string()), (2, "square".to_string())];
    let hits = fuzzy_match("xyz", &candidates, 10);
    assert!(hits.is_empty());
}

#[test]
fn exact_contiguous_substring_match_outscores_scattered_subsequence_match() {
    let candidates = vec![(1i64, "concatenate".to_string()), (2, "c9a9t".to_string())];
    let hits = fuzzy_match("cat", &candidates, 10);
    assert_eq!(hits.len(), 2);
    assert!(score_for(&hits, 1) > score_for(&hits, 2));
    assert_eq!(hits.first().unwrap().node_id, 1);
}

#[test]
fn word_boundary_match_outscores_mid_word_match_of_same_shape() {
    let candidates = vec![(1i64, "foo_square".to_string()), (2, "foobsquare".to_string())];
    let hits = fuzzy_match("sq", &candidates, 10);
    assert_eq!(hits.len(), 2);
    assert!(score_for(&hits, 1) > score_for(&hits, 2));
    assert_eq!(hits.first().unwrap().node_id, 1);
}

#[test]
fn camel_case_boundary_match_outscores_mid_word_match_of_same_shape() {
    let candidates = vec![(1i64, "getSquareRoot".to_string()), (2, "isquarefoo".to_string())];
    let hits = fuzzy_match("sq", &candidates, 10);
    assert_eq!(hits.len(), 2);
    assert!(score_for(&hits, 1) > score_for(&hits, 2));
}

#[test]
fn matching_at_index_0_scores_higher_than_otherwise_identical_match_starting_later() {
    let candidates = vec![(1i64, "square".to_string()), (2, "xsquare".to_string())];
    let hits = fuzzy_match("square", &candidates, 10);
    assert_eq!(hits.len(), 2);
    assert!(score_for(&hits, 1) > score_for(&hits, 2));
}

#[test]
fn ties_are_broken_by_shorter_candidate_text_then_lower_node_id_deterministically() {
    let candidates = vec![
        (30i64, "xxsquare".to_string()), // len 8
        (10, "square".to_string()),      // len 6, lower id
        (20, "square".to_string()),      // len 6, higher id
    ];
    let hits = fuzzy_match("square", &candidates, 10);
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].node_id, 10);
    assert_eq!(hits[1].node_id, 20);
    assert_eq!(hits[2].node_id, 30);
}

#[test]
fn results_are_deterministic_across_repeated_calls_with_same_input() {
    let candidates = vec![
        (1i64, "Square".to_string()),
        (2, "fixture::Square".to_string()),
        (3, "SquareRoot".to_string()),
        (4, "sq".to_string()),
        (5, "backSquare".to_string()),
        (6, "square".to_string()),
    ];
    let first = fuzzy_match("sq", &candidates, 10);
    for _ in 0..5 {
        let again = fuzzy_match("sq", &candidates, 10);
        assert_eq!(again.len(), first.len());
        for j in 0..first.len() {
            assert_eq!(again[j].node_id, first[j].node_id);
            assert_eq!(again[j].score, first[j].score);
        }
    }
}

#[test]
fn limit_truncates_the_already_sorted_result() {
    let candidates = vec![
        (1i64, "square".to_string()),
        (2, "squareroot".to_string()),
        (3, "squarish".to_string()),
        (4, "esquire".to_string()),
    ];
    let hits = fuzzy_match("squ", &candidates, 2);
    assert_eq!(hits.len(), 2);
}

#[test]
fn matched_indices_refer_to_positions_in_original_non_lowercased_text() {
    let candidates = vec![(1i64, "Square".to_string())];
    let hits = fuzzy_match("sq", &candidates, 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_indices.len(), 2);
    assert_eq!(hits[0].matched_indices[0], 0);
    assert_eq!(hits[0].matched_indices[1], 1);
}