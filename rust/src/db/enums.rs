//! Bidirectional int<->string tables for the ACTUAL integer values
//! Sourcetrail's schema stores in `node.type` / `edge.type` /
//! `symbol.definition_kind` / `source_location.type`.
//!
//! These are copied directly from the vendored headers
//! (third_party/sourcetraildb/include/{NodeKind,EdgeKind,DefinitionKind,
//! LocationKind}.h) — NOT reconstructed from memory — since those values are
//! what's literally on disk in any `.srctrldb`.

/// `node.type` values (third_party/sourcetraildb/include/NodeKind.h).
pub mod node_kind {
    pub const UNKNOWN: i32 = 1 << 0;
    pub const TYPE: i32 = 1 << 1;
    pub const BUILTIN_TYPE: i32 = 1 << 2;
    pub const MODULE: i32 = 1 << 3;
    pub const NAMESPACE: i32 = 1 << 4;
    pub const PACKAGE: i32 = 1 << 5;
    pub const STRUCT: i32 = 1 << 6;
    pub const CLASS: i32 = 1 << 7;
    pub const INTERFACE: i32 = 1 << 8;
    pub const ANNOTATION: i32 = 1 << 9;
    pub const GLOBAL_VARIABLE: i32 = 1 << 10;
    pub const FIELD: i32 = 1 << 11;
    pub const FUNCTION: i32 = 1 << 12;
    pub const METHOD: i32 = 1 << 13;
    pub const ENUM: i32 = 1 << 14;
    pub const ENUM_CONSTANT: i32 = 1 << 15;
    pub const TYPEDEF: i32 = 1 << 16;
    pub const TYPE_PARAMETER: i32 = 1 << 17;
    pub const FILE: i32 = 1 << 18;
    pub const MACRO: i32 = 1 << 19;
    pub const UNION: i32 = 1 << 20;
}

/// `edge.type` values (third_party/sourcetraildb/include/EdgeKind.h).
pub mod edge_kind {
    pub const UNKNOWN: i32 = 0;
    pub const MEMBER: i32 = 1 << 0;
    pub const TYPE_USAGE: i32 = 1 << 1;
    pub const USAGE: i32 = 1 << 2;
    pub const CALL: i32 = 1 << 3;
    pub const INHERITANCE: i32 = 1 << 4;
    pub const OVERRIDE: i32 = 1 << 5;
    pub const TYPE_ARGUMENT: i32 = 1 << 6;
    pub const TEMPLATE_SPECIALIZATION: i32 = 1 << 7;
    pub const INCLUDE: i32 = 1 << 8;
    pub const IMPORT: i32 = 1 << 9;
    pub const MACRO_USAGE: i32 = 1 << 11;
    pub const ANNOTATION_USAGE: i32 = 1 << 12;

    /// OR of every known edge kind bit; the "no filter" mask.
    pub const ALL: i32 = MEMBER
        | TYPE_USAGE
        | USAGE
        | CALL
        | INHERITANCE
        | OVERRIDE
        | TYPE_ARGUMENT
        | TEMPLATE_SPECIALIZATION
        | INCLUDE
        | IMPORT
        | MACRO_USAGE
        | ANNOTATION_USAGE;
}

/// `symbol.definition_kind` values (third_party/sourcetraildb/include/DefinitionKind.h).
pub mod definition_kind {
    pub const IMPLICIT: i32 = 1;
    pub const EXPLICIT: i32 = 2;
}

/// `source_location.type` values (third_party/sourcetraildb/include/LocationKind.h).
pub mod location_kind {
    pub const TOKEN: i32 = 0;
    pub const SCOPE: i32 = 1;
    pub const QUALIFIER: i32 = 2;
    pub const LOCAL_SYMBOL: i32 = 3;
    pub const SIGNATURE: i32 = 4;
    pub const ATOMIC_RANGE: i32 = 5;
    pub const INDEXER_ERROR: i32 = 6;
    pub const FULLTEXT_SEARCH: i32 = 7;
    pub const SCREEN_SEARCH: i32 = 8;
    pub const UNSOLVED: i32 = 9;
}

struct Entry {
    value: i32,
    name: &'static str,
}

const NODE_KIND_TABLE: [Entry; 21] = [
    Entry { value: node_kind::UNKNOWN, name: "UNKNOWN" },
    Entry { value: node_kind::TYPE, name: "TYPE" },
    Entry { value: node_kind::BUILTIN_TYPE, name: "BUILTIN_TYPE" },
    Entry { value: node_kind::MODULE, name: "MODULE" },
    Entry { value: node_kind::NAMESPACE, name: "NAMESPACE" },
    Entry { value: node_kind::PACKAGE, name: "PACKAGE" },
    Entry { value: node_kind::STRUCT, name: "STRUCT" },
    Entry { value: node_kind::CLASS, name: "CLASS" },
    Entry { value: node_kind::INTERFACE, name: "INTERFACE" },
    Entry { value: node_kind::ANNOTATION, name: "ANNOTATION" },
    Entry { value: node_kind::GLOBAL_VARIABLE, name: "GLOBAL_VARIABLE" },
    Entry { value: node_kind::FIELD, name: "FIELD" },
    Entry { value: node_kind::FUNCTION, name: "FUNCTION" },
    Entry { value: node_kind::METHOD, name: "METHOD" },
    Entry { value: node_kind::ENUM, name: "ENUM" },
    Entry { value: node_kind::ENUM_CONSTANT, name: "ENUM_CONSTANT" },
    Entry { value: node_kind::TYPEDEF, name: "TYPEDEF" },
    Entry { value: node_kind::TYPE_PARAMETER, name: "TYPE_PARAMETER" },
    Entry { value: node_kind::FILE, name: "FILE" },
    Entry { value: node_kind::MACRO, name: "MACRO" },
    Entry { value: node_kind::UNION, name: "UNION" },
];

const EDGE_KIND_TABLE: [Entry; 13] = [
    Entry { value: edge_kind::UNKNOWN, name: "UNKNOWN" },
    Entry { value: edge_kind::MEMBER, name: "MEMBER" },
    Entry { value: edge_kind::TYPE_USAGE, name: "TYPE_USAGE" },
    Entry { value: edge_kind::USAGE, name: "USAGE" },
    Entry { value: edge_kind::CALL, name: "CALL" },
    Entry { value: edge_kind::INHERITANCE, name: "INHERITANCE" },
    Entry { value: edge_kind::OVERRIDE, name: "OVERRIDE" },
    Entry { value: edge_kind::TYPE_ARGUMENT, name: "TYPE_ARGUMENT" },
    Entry { value: edge_kind::TEMPLATE_SPECIALIZATION, name: "TEMPLATE_SPECIALIZATION" },
    Entry { value: edge_kind::INCLUDE, name: "INCLUDE" },
    Entry { value: edge_kind::IMPORT, name: "IMPORT" },
    Entry { value: edge_kind::MACRO_USAGE, name: "MACRO_USAGE" },
    Entry { value: edge_kind::ANNOTATION_USAGE, name: "ANNOTATION_USAGE" },
];

const DEFINITION_KIND_TABLE: [Entry; 2] = [
    Entry { value: definition_kind::IMPLICIT, name: "IMPLICIT" },
    Entry { value: definition_kind::EXPLICIT, name: "EXPLICIT" },
];

const LOCATION_KIND_TABLE: [Entry; 10] = [
    Entry { value: location_kind::TOKEN, name: "TOKEN" },
    Entry { value: location_kind::SCOPE, name: "SCOPE" },
    Entry { value: location_kind::QUALIFIER, name: "QUALIFIER" },
    Entry { value: location_kind::LOCAL_SYMBOL, name: "LOCAL_SYMBOL" },
    Entry { value: location_kind::SIGNATURE, name: "SIGNATURE" },
    Entry { value: location_kind::ATOMIC_RANGE, name: "ATOMIC_RANGE" },
    Entry { value: location_kind::INDEXER_ERROR, name: "INDEXER_ERROR" },
    Entry { value: location_kind::FULLTEXT_SEARCH, name: "FULLTEXT_SEARCH" },
    Entry { value: location_kind::SCREEN_SEARCH, name: "SCREEN_SEARCH" },
    Entry { value: location_kind::UNSOLVED, name: "UNSOLVED" },
];

fn to_string_impl(table: &[Entry], value: i32) -> String {
    for e in table {
        if e.value == value {
            return e.name.to_string();
        }
    }
    format!("UNKNOWN({})", value)
}

fn from_string_impl(table: &[Entry], name: &str) -> Option<i32> {
    let upper = name.to_ascii_uppercase();
    for e in table {
        if upper == e.name {
            return Some(e.value);
        }
    }
    None
}

/// Node kinds Bfs treats as "namespace-like" (pure containers traversed
/// through but never themselves a filtered result).
pub fn is_namespace_like_node_kind(kind: i32) -> bool {
    kind == node_kind::NAMESPACE || kind == node_kind::MODULE || kind == node_kind::PACKAGE
}

pub fn node_kind_to_string(kind: i32) -> String {
    to_string_impl(&NODE_KIND_TABLE, kind)
}

pub fn edge_kind_to_string(kind: i32) -> String {
    to_string_impl(&EDGE_KIND_TABLE, kind)
}

pub fn definition_kind_to_string(kind: i32) -> String {
    to_string_impl(&DEFINITION_KIND_TABLE, kind)
}

pub fn location_kind_to_string(kind: i32) -> String {
    to_string_impl(&LOCATION_KIND_TABLE, kind)
}

pub fn node_kind_from_string(name: &str) -> Option<i32> {
    from_string_impl(&NODE_KIND_TABLE, name)
}

pub fn edge_kind_from_string(name: &str) -> Option<i32> {
    from_string_impl(&EDGE_KIND_TABLE, name)
}

pub fn definition_kind_from_string(name: &str) -> Option<i32> {
    from_string_impl(&DEFINITION_KIND_TABLE, name)
}

pub fn location_kind_from_string(name: &str) -> Option<i32> {
    from_string_impl(&LOCATION_KIND_TABLE, name)
}