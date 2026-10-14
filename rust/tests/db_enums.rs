//! Additional coverage for the db::enums int<->string tables (mirrors the
//! C++ `symhow::db::Enums` free functions; the C++ suite covers these
//! indirectly, so this pins them directly for the Rust port).

use symhow::db::enums::*;

#[test]
fn node_kind_to_string_round_trips_known_values() {
    assert_eq!(node_kind_to_string(node_kind::NAMESPACE), "NAMESPACE");
    assert_eq!(node_kind_to_string(node_kind::CLASS), "CLASS");
    assert_eq!(node_kind_to_string(node_kind::FUNCTION), "FUNCTION");
    assert_eq!(node_kind_to_string(node_kind::UNION), "UNION");
    assert_eq!(node_kind_to_string(0xdead), "UNKNOWN(57005)");
}

#[test]
fn edge_kind_to_string_round_trips_known_values() {
    assert_eq!(edge_kind_to_string(edge_kind::CALL), "CALL");
    assert_eq!(edge_kind_to_string(edge_kind::INHERITANCE), "INHERITANCE");
    assert_eq!(edge_kind_to_string(edge_kind::MACRO_USAGE), "MACRO_USAGE");
    assert_eq!(edge_kind_to_string(9999), "UNKNOWN(9999)");
}

#[test]
fn from_string_is_case_insensitive_and_rejects_unknown() {
    assert_eq!(node_kind_from_string("class"), Some(node_kind::CLASS));
    assert_eq!(node_kind_from_string("CLASS"), Some(node_kind::CLASS));
    assert_eq!(node_kind_from_string("ClAsS"), Some(node_kind::CLASS));
    assert_eq!(node_kind_from_string("not_a_kind"), None);

    assert_eq!(edge_kind_from_string("call"), Some(edge_kind::CALL));
    assert_eq!(definition_kind_from_string("EXPLICIT"), Some(definition_kind::EXPLICIT));
    assert_eq!(location_kind_from_string("token"), Some(location_kind::TOKEN));
    assert_eq!(location_kind_from_string("bogus"), None);
}

#[test]
fn namespace_like_predicate_marks_only_container_kinds() {
    assert!(is_namespace_like_node_kind(node_kind::NAMESPACE));
    assert!(is_namespace_like_node_kind(node_kind::MODULE));
    assert!(is_namespace_like_node_kind(node_kind::PACKAGE));
    assert!(!is_namespace_like_node_kind(node_kind::CLASS));
    assert!(!is_namespace_like_node_kind(node_kind::FUNCTION));
}

#[test]
fn edge_kind_all_is_full_bitmask() {
    let all = edge_kind::ALL;
    for bit in [
        edge_kind::MEMBER,
        edge_kind::TYPE_USAGE,
        edge_kind::USAGE,
        edge_kind::CALL,
        edge_kind::INHERITANCE,
        edge_kind::OVERRIDE,
        edge_kind::TYPE_ARGUMENT,
        edge_kind::TEMPLATE_SPECIALIZATION,
        edge_kind::INCLUDE,
        edge_kind::IMPORT,
        edge_kind::MACRO_USAGE,
        edge_kind::ANNOTATION_USAGE,
    ] {
        assert_eq!(all & bit, bit);
    }
}