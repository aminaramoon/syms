// Round-trips symhow::db::decodeSerializedName() against both hand-constructed
// serialized_name strings (mirroring exactly what
// sourcetrail::serializeNameHierarchyToDatabaseString() produces -- see
// third_party/sourcetraildb/src/NameHierarchy.cpp) and real values pulled
// directly from build/fixture.srctrldb via `sqlite3` during development
// (hard-coded below rather than re-querying the DB at test time, so this
// file has no FixtureDb dependency and runs unconditionally).
//
// Real examples were captured with e.g.:
//   sqlite3 build/fixture.srctrldb "SELECT id, serialized_name FROM node WHERE serialized_name LIKE '%Square%'"
// which is how the "Square" class/ctor/area and tu_a/tu_b helper/main
// strings below were obtained.

#include <catch2/catch_test_macros.hpp>

#include "symhow/db/NameDecode.h"

using symhow::db::decodeSerializedName;

namespace
{

// Real fixture.srctrldb serialized_name values (node ids from a fresh
// `ctest -R fixture_index` run; ids may drift across re-indexing, the
// strings themselves are what's under test here, not the ids).
constexpr const char* kSquareClass = "::\tmfixture\ts\tp\tnSquare\ts\tp";
constexpr const char* kSquareCtor = "::\tmfixture\ts\tp\tnSquare\ts\tp\tnSquare\ts\tp(double)";
constexpr const char* kSquareArea = "::\tmfixture\ts\tp\tnSquare\ts\tp\tnarea\tsdouble \tp() const";
constexpr const char* kTuAHelper = "::\tmhelper\tsvoid \tp() [tu_a.cpp#317677a3]";
constexpr const char* kTuBHelper = "::\tmhelper\tsvoid \tp() [tu_b.cpp#c490d472]";
constexpr const char* kTuAMain = "::\tmmain\tsint \tp() [tu_a.cpp#317677a3]";

}  // namespace

TEST_CASE("name_decode round-trips a plain two-level class name (real: fixture::Square)", "[name_decode]")
{
    const symhow::db::DecodedName d = decodeSerializedName(kSquareClass);
    REQUIRE(d.nameDelimiter == "::");
    REQUIRE(d.elements.size() == 2);
    CHECK(d.elements[0].name == "fixture");
    CHECK(d.elements[0].prefix.empty());
    CHECK(d.elements[0].postfix.empty());
    CHECK(d.elements[1].name == "Square");

    CHECK(d.namePath() == "fixture::Square");
    CHECK(d.qualifiedName() == "fixture::Square");
    CHECK(d.displayName() == "fixture::Square");
}

TEST_CASE("name_decode keeps a function-signature postfix out of namePath but in qualifiedName (real: Square ctor)", "[name_decode]")
{
    const symhow::db::DecodedName d = decodeSerializedName(kSquareCtor);
    REQUIRE(d.elements.size() == 3);
    CHECK(d.elements.back().name == "Square");
    CHECK(d.elements.back().postfix == "(double)");

    CHECK(d.namePath() == "fixture::Square::Square");
    CHECK(d.qualifiedName() == "fixture::Square::Square(double)");
    CHECK(d.displayName() == "fixture::Square::Square(double)");
}

TEST_CASE("name_decode decodes a prefix+postfix method signature (real: Square::area)", "[name_decode]")
{
    const symhow::db::DecodedName d = decodeSerializedName(kSquareArea);
    REQUIRE(d.elements.size() == 3);
    CHECK(d.elements.back().prefix == "double ");
    CHECK(d.elements.back().name == "area");
    CHECK(d.elements.back().postfix == "() const");

    CHECK(d.namePath() == "fixture::Square::area");
    CHECK(d.qualifiedName() == "fixture::Square::area() const");
    CHECK(d.displayName() == "fixture::Square::area() const");
    CHECK(d.signature() == "double  area() const");
}

TEST_CASE("name_decode strips Phase 3's TU-local disambiguator from displayName but keeps it in qualifiedName (real: tu_a/tu_b helper)", "[name_decode]")
{
    const symhow::db::DecodedName a = decodeSerializedName(kTuAHelper);
    const symhow::db::DecodedName b = decodeSerializedName(kTuBHelper);

    CHECK(a.qualifiedName() == "helper() [tu_a.cpp#317677a3]");
    CHECK(b.qualifiedName() == "helper() [tu_b.cpp#c490d472]");

    // The whole point of the disambiguator: two different TUs' same-named
    // internal-linkage statics get DIFFERENT qualifiedName()s...
    CHECK(a.qualifiedName() != b.qualifiedName());

    // ...but the SAME, human-friendly displayName() once it's stripped.
    CHECK(a.displayName() == "helper()");
    CHECK(b.displayName() == "helper()");
    CHECK(a.displayName() == b.displayName());
}

TEST_CASE("name_decode disambiguator stripping also works on a return-type-prefixed function (real: tu_a main)", "[name_decode]")
{
    const symhow::db::DecodedName d = decodeSerializedName(kTuAMain);
    CHECK(d.elements.back().prefix == "int ");
    CHECK(d.qualifiedName() == "main() [tu_a.cpp#317677a3]");
    CHECK(d.displayName() == "main()");
    CHECK(d.signature() == "int  main() [tu_a.cpp#317677a3]");
}

TEST_CASE("name_decode hand-constructed: multi-element hierarchy with a non-empty leaf prefix/postfix", "[name_decode]")
{
    // Mirrors serializeNameHierarchyToDatabaseString()'s exact grammar:
    //   nameDelimiter + "\tm" + name0 + "\ts" + prefix0 + "\tp" + postfix0
    //     + "\tn" + name1 + "\ts" + prefix1 + "\tp" + postfix1 + ...
    const std::string serialized = "::\tmouter\ts\tp\tninner\tsPfx \tp[]";
    const symhow::db::DecodedName d = decodeSerializedName(serialized);

    REQUIRE(d.nameDelimiter == "::");
    REQUIRE(d.elements.size() == 2);
    CHECK(d.elements[0].name == "outer");
    CHECK(d.elements[1].prefix == "Pfx ");
    CHECK(d.elements[1].name == "inner");
    CHECK(d.elements[1].postfix == "[]");

    CHECK(d.namePath() == "outer::inner");
    CHECK(d.qualifiedName() == "outer::inner[]");
    CHECK(d.signature() == "Pfx  inner[]");

    // "[]" does NOT match the disambiguator pattern (` [<basename>#<8 hex
    // digits>]` at the very end) -- a legitimate trailing bracket postfix
    // (e.g. an array-type spelling) must never be truncated.
    CHECK(d.displayName() == d.qualifiedName());
}

TEST_CASE("name_decode does not strip a bracket suffix that almost, but doesn't exactly, match the disambiguator pattern", "[name_decode]")
{
    // Only 4 hex digits, not 8 -- must not be treated as a disambiguator.
    const std::string notQuiteDisambig = "::\tmfoo\tsvoid \tp() [x#1234]";
    const symhow::db::DecodedName d1 = decodeSerializedName(notQuiteDisambig);
    CHECK(d1.displayName() == d1.qualifiedName());
    CHECK(d1.displayName() == "foo() [x#1234]");

    // No trailing "#" at all.
    const std::string noHash = "::\tmfoo\tsvoid \tp() [array of 4]";
    const symhow::db::DecodedName d2 = decodeSerializedName(noHash);
    CHECK(d2.displayName() == d2.qualifiedName());
}

TEST_CASE("name_decode single-element (no ancestors) name, e.g. a free function", "[name_decode]")
{
    const std::string serialized = "::\tmlevel0\tsint \tp()";
    const symhow::db::DecodedName d = decodeSerializedName(serialized);
    REQUIRE(d.elements.size() == 1);
    CHECK(d.namePath() == "level0");
    CHECK(d.qualifiedName() == "level0()");
    CHECK(d.displayName() == "level0()");
    CHECK(d.signature() == "int  level0()");
}
