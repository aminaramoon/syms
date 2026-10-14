//! Port of test/unit/test_name_decode.cpp.

use symhow::db::name_decode::decode_serialized_name;

const SQUARE_CLASS: &str = "::\tmfixture\ts\tp\tnSquare\ts\tp";
const SQUARE_CTOR: &str = "::\tmfixture\ts\tp\tnSquare\ts\tp\tnSquare\ts\tp(double)";
const SQUARE_AREA: &str = "::\tmfixture\ts\tp\tnSquare\ts\tp\tnarea\tsdouble \tp() const";
const TU_A_HELPER: &str = "::\tmhelper\tsvoid \tp() [tu_a.cpp#317677a3]";
const TU_B_HELPER: &str = "::\tmhelper\tsvoid \tp() [tu_b.cpp#c490d472]";
const TU_A_MAIN: &str = "::\tmmain\tsint \tp() [tu_a.cpp#317677a3]";

#[test]
fn round_trips_a_plain_two_level_class_name() {
    let d = decode_serialized_name(SQUARE_CLASS);
    assert_eq!(d.name_delimiter, "::");
    assert_eq!(d.elements.len(), 2);
    assert_eq!(d.elements[0].name, "fixture");
    assert!(d.elements[0].prefix.is_empty());
    assert!(d.elements[0].postfix.is_empty());
    assert_eq!(d.elements[1].name, "Square");

    assert_eq!(d.name_path(), "fixture::Square");
    assert_eq!(d.qualified_name(), "fixture::Square");
    assert_eq!(d.display_name(), "fixture::Square");
}

#[test]
fn keeps_function_signature_postfix_out_of_name_path_but_in_qualified_name() {
    let d = decode_serialized_name(SQUARE_CTOR);
    assert_eq!(d.elements.len(), 3);
    assert_eq!(d.elements.last().unwrap().name, "Square");
    assert_eq!(d.elements.last().unwrap().postfix, "(double)");

    assert_eq!(d.name_path(), "fixture::Square::Square");
    assert_eq!(d.qualified_name(), "fixture::Square::Square(double)");
    assert_eq!(d.display_name(), "fixture::Square::Square(double)");
}

#[test]
fn decodes_a_prefix_plus_postfix_method_signature() {
    let d = decode_serialized_name(SQUARE_AREA);
    assert_eq!(d.elements.len(), 3);
    assert_eq!(d.elements.last().unwrap().prefix, "double ");
    assert_eq!(d.elements.last().unwrap().name, "area");
    assert_eq!(d.elements.last().unwrap().postfix, "() const");

    assert_eq!(d.name_path(), "fixture::Square::area");
    assert_eq!(d.qualified_name(), "fixture::Square::area() const");
    assert_eq!(d.display_name(), "fixture::Square::area() const");
    assert_eq!(d.signature(), "double  area() const");
}

#[test]
fn strips_tu_local_disambiguator_from_display_name_but_keeps_in_qualified_name() {
    let a = decode_serialized_name(TU_A_HELPER);
    let b = decode_serialized_name(TU_B_HELPER);

    assert_eq!(a.qualified_name(), "helper() [tu_a.cpp#317677a3]");
    assert_eq!(b.qualified_name(), "helper() [tu_b.cpp#c490d472]");
    assert_ne!(a.qualified_name(), b.qualified_name());

    assert_eq!(a.display_name(), "helper()");
    assert_eq!(b.display_name(), "helper()");
    assert_eq!(a.display_name(), b.display_name());
}

#[test]
fn disambiguator_stripping_works_on_return_type_prefixed_function() {
    let d = decode_serialized_name(TU_A_MAIN);
    assert_eq!(d.elements.last().unwrap().prefix, "int ");
    assert_eq!(d.qualified_name(), "main() [tu_a.cpp#317677a3]");
    assert_eq!(d.display_name(), "main()");
    assert_eq!(d.signature(), "int  main() [tu_a.cpp#317677a3]");
}

#[test]
fn multi_element_hierarchy_with_non_empty_leaf_prefix_postfix() {
    let serialized = "::\tmouter\ts\tp\tninner\tsPfx \tp[]";
    let d = decode_serialized_name(serialized);

    assert_eq!(d.name_delimiter, "::");
    assert_eq!(d.elements.len(), 2);
    assert_eq!(d.elements[0].name, "outer");
    assert_eq!(d.elements[1].prefix, "Pfx ");
    assert_eq!(d.elements[1].name, "inner");
    assert_eq!(d.elements[1].postfix, "[]");

    assert_eq!(d.name_path(), "outer::inner");
    assert_eq!(d.qualified_name(), "outer::inner[]");
    assert_eq!(d.signature(), "Pfx  inner[]");
    assert_eq!(d.display_name(), d.qualified_name());
}

#[test]
fn does_not_strip_bracket_suffix_that_almost_matches_disambiguator() {
    let d1 = decode_serialized_name("::\tmfoo\tsvoid \tp() [x#1234]");
    assert_eq!(d1.display_name(), d1.qualified_name());
    assert_eq!(d1.display_name(), "foo() [x#1234]");

    let d2 = decode_serialized_name("::\tmfoo\tsvoid \tp() [array of 4]");
    assert_eq!(d2.display_name(), d2.qualified_name());
}

#[test]
fn single_element_name() {
    let d = decode_serialized_name("::\tmlevel0\tsint \tp()");
    assert_eq!(d.elements.len(), 1);
    assert_eq!(d.name_path(), "level0");
    assert_eq!(d.qualified_name(), "level0()");
    assert_eq!(d.display_name(), "level0()");
    assert_eq!(d.signature(), "int  level0()");
}