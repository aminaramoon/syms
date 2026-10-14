#pragma once

#include <optional>
#include <string>

// Bidirectional int<->string tables for the ACTUAL integer values Sourcetrail's
// schema stores in node.type / edge.type / symbol.definition_kind /
// source_location.type. These are copied directly from the vendored headers
// (third_party/sourcetraildb/include/{NodeKind,EdgeKind,DefinitionKind,
// LocationKind}.h) -- NOT reconstructed from memory -- since those values are
// what's literally on disk in any .srctrldb (whether written by `symhow index` or
// by the real Sourcetrail GUI/indexer).
//
// Deliberately plain `namespace X { constexpr int Y = ...; }` blocks rather
// than C++ `enum class`: every value read out of SQLite arrives as a bare
// `int`/`int64_t` column, and query code (Bfs, QueryEngine) constantly does
// bitmask arithmetic (`type & mask`) and OR-combination of multiple
// `--kind`/`--edge-kind` flags -- both awkward with a scoped enum without
// constant `static_cast`-ing everywhere. The string tables below use the
// upstream enumerator spelling (e.g. "NAMESPACE", "CALL") since that's what
// Phase 5's JSON schema wants, and are exposed as free functions so text
// output (this phase) and JSON output (Phase 5) can share them.
namespace symhow::db
{

// node.type values (third_party/sourcetraildb/include/NodeKind.h).
namespace NodeKind
{
constexpr int UNKNOWN = 1 << 0;
constexpr int TYPE = 1 << 1;
constexpr int BUILTIN_TYPE = 1 << 2;
constexpr int MODULE = 1 << 3;
constexpr int NAMESPACE = 1 << 4;
constexpr int PACKAGE = 1 << 5;
constexpr int STRUCT = 1 << 6;
constexpr int CLASS = 1 << 7;
constexpr int INTERFACE = 1 << 8;
constexpr int ANNOTATION = 1 << 9;
constexpr int GLOBAL_VARIABLE = 1 << 10;
constexpr int FIELD = 1 << 11;
constexpr int FUNCTION = 1 << 12;
constexpr int METHOD = 1 << 13;
constexpr int ENUM = 1 << 14;
constexpr int ENUM_CONSTANT = 1 << 15;
constexpr int TYPEDEF = 1 << 16;
constexpr int TYPE_PARAMETER = 1 << 17;
constexpr int FILE = 1 << 18;
constexpr int MACRO = 1 << 19;
constexpr int UNION = 1 << 20;
}  // namespace NodeKind

// edge.type values (third_party/sourcetraildb/include/EdgeKind.h).
namespace EdgeKind
{
constexpr int UNKNOWN = 0;
constexpr int MEMBER = 1 << 0;
constexpr int TYPE_USAGE = 1 << 1;
constexpr int USAGE = 1 << 2;
constexpr int CALL = 1 << 3;
constexpr int INHERITANCE = 1 << 4;
constexpr int OVERRIDE = 1 << 5;
constexpr int TYPE_ARGUMENT = 1 << 6;
constexpr int TEMPLATE_SPECIALIZATION = 1 << 7;
constexpr int INCLUDE = 1 << 8;
constexpr int IMPORT = 1 << 9;
constexpr int MACRO_USAGE = 1 << 11;
constexpr int ANNOTATION_USAGE = 1 << 12;

// OR of every known edge kind bit; useful as "no filter" mask for verbs like
// `neighbors` that default to "all kinds".
constexpr int ALL = MEMBER | TYPE_USAGE | USAGE | CALL | INHERITANCE | OVERRIDE | TYPE_ARGUMENT | TEMPLATE_SPECIALIZATION |
                     INCLUDE | IMPORT | MACRO_USAGE | ANNOTATION_USAGE;
}  // namespace EdgeKind

// symbol.definition_kind values (third_party/sourcetraildb/include/DefinitionKind.h).
// Note there is no "0"/NONE value upstream: a symbol with no recorded
// definition kind simply has no `symbol` table row at all (see
// QueryEngine::search()'s `definition_kind IS NULL` handling).
namespace DefinitionKind
{
constexpr int IMPLICIT = 1;
constexpr int EXPLICIT = 2;
}  // namespace DefinitionKind

// source_location.type values (third_party/sourcetraildb/include/LocationKind.h).
namespace LocationKind
{
constexpr int TOKEN = 0;
constexpr int SCOPE = 1;
constexpr int QUALIFIER = 2;
constexpr int LOCAL_SYMBOL = 3;
constexpr int SIGNATURE = 4;
constexpr int ATOMIC_RANGE = 5;
constexpr int INDEXER_ERROR = 6;
constexpr int FULLTEXT_SEARCH = 7;
constexpr int SCREEN_SEARCH = 8;
constexpr int UNSOLVED = 9;
}  // namespace LocationKind

// Node kinds that Bfs treats as "namespace-like" for the purposes of the
// containment-hop pass-through rule (excluded from a node-kind-filtered
// result's rendered node list, but still traversed through so e.g.
// `--kind CLASS` doesn't sever a chain that happens to pass through a
// namespace). Only NAMESPACE is ever produced by `symhow index`; MODULE/PACKAGE
// are included defensively since they're the same kind of "pure container,
// never itself the thing you asked for" node in the upstream schema.
bool isNamespaceLikeNodeKind(int kind);

// Upstream enumerator name (e.g. "NAMESPACE", "CALL"), or "UNKNOWN(<n>)" for
// a value with no corresponding constant above (should not happen for a
// well-formed database, but must never throw -- this is used by text/JSON
// output on every row).
std::string nodeKindToString(int kind);
std::string edgeKindToString(int kind);
std::string definitionKindToString(int kind);
std::string locationKindToString(int kind);

// Case-insensitive reverse lookups, for CLI flag parsing (--kind CLASS,
// --edge-kind CALL, --location-kind TOKEN). std::nullopt if `name` doesn't
// match any known enumerator.
std::optional<int> nodeKindFromString(const std::string& name);
std::optional<int> edgeKindFromString(const std::string& name);
std::optional<int> definitionKindFromString(const std::string& name);
std::optional<int> locationKindFromString(const std::string& name);

}  // namespace symhow::db
