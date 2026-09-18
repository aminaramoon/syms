#include <catch2/catch_test_macros.hpp>

#include <algorithm>

#include "kg/query/FuzzyMatcher.h"

using kg::query::FuzzyHit;
using kg::query::fuzzyMatch;

namespace
{

int scoreFor(const std::vector<FuzzyHit>& hits, int64_t id)
{
    for (const FuzzyHit& h : hits)
    {
        if (h.nodeId == id)
        {
            return h.score;
        }
    }
    FAIL("candidate id " << id << " did not match at all");
    return 0;
}

}  // namespace

TEST_CASE("fuzzy: a candidate that never contains the pattern as a subsequence does not match", "[fuzzy]")
{
    std::vector<std::pair<int64_t, std::string>> candidates{{1, "banana"}, {2, "square"}};
    auto hits = fuzzyMatch("xyz", candidates, 10);
    CHECK(hits.empty());
}

TEST_CASE("fuzzy: an exact contiguous substring match outscores a scattered subsequence match", "[fuzzy]")
{
    // Both contain "cat" as an in-order subsequence; candidate 1 has it as a
    // contiguous run ("con-CAT-enate"), candidate 2 has it scattered across
    // unrelated filler characters with no word-boundary characters to
    // artificially inflate its score.
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "concatenate"},
        {2, "c9a9t"},
    };
    auto hits = fuzzyMatch("cat", candidates, 10);
    REQUIRE(hits.size() == 2);
    CHECK(scoreFor(hits, 1) > scoreFor(hits, 2));
    // And the sort order reflects it (descending by score).
    CHECK(hits.front().nodeId == 1);
}

TEST_CASE("fuzzy: a word-boundary match outscores a mid-word match of the same shape", "[fuzzy]")
{
    // Both match "sq" at index 4 with a contiguous run; the only difference
    // is the character immediately before index 4: '_' (a boundary char) for
    // candidate 1, 'b' (not a boundary char) for candidate 2.
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "foo_square"},
        {2, "foobsquare"},
    };
    auto hits = fuzzyMatch("sq", candidates, 10);
    REQUIRE(hits.size() == 2);
    CHECK(scoreFor(hits, 1) > scoreFor(hits, 2));
    CHECK(hits.front().nodeId == 1);
}

TEST_CASE("fuzzy: a camelCase boundary match outscores a mid-word match of the same shape", "[fuzzy]")
{
    // "sq" matches the 'S' at index 3 in "getSquareRoot" (camelCase boundary:
    // preceded by lowercase 't'), vs "isquare" where 's' at index 1 is
    // preceded by lowercase 'i' with no case change.
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "getSquareRoot"},
        {2, "isquarefoo"},
    };
    auto hits = fuzzyMatch("sq", candidates, 10);
    REQUIRE(hits.size() == 2);
    CHECK(scoreFor(hits, 1) > scoreFor(hits, 2));
}

TEST_CASE("fuzzy: matching at index 0 scores higher than an otherwise-identical match starting later", "[fuzzy]")
{
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "square"},
        {2, "xsquare"},
    };
    auto hits = fuzzyMatch("square", candidates, 10);
    REQUIRE(hits.size() == 2);
    CHECK(scoreFor(hits, 1) > scoreFor(hits, 2));
}

TEST_CASE("fuzzy: ties are broken by shorter candidate text, then lower nodeId, deterministically", "[fuzzy]")
{
    // All three are identical strings at different ids/lengths so they tie
    // on score; the tie-break must be (textLength asc, nodeId asc).
    std::vector<std::pair<int64_t, std::string>> candidates{
        {30, "xxsquare"},  // len 8
        {10, "square"},    // len 6, lower id
        {20, "square"},    // len 6, higher id
    };
    auto hits = fuzzyMatch("square", candidates, 10);
    REQUIRE(hits.size() == 3);
    CHECK(hits[0].nodeId == 10);
    CHECK(hits[1].nodeId == 20);
    CHECK(hits[2].nodeId == 30);
}

TEST_CASE("fuzzy: results are deterministic across repeated calls with the same input", "[fuzzy]")
{
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "Square"}, {2, "fixture::Square"}, {3, "SquareRoot"}, {4, "sq"}, {5, "backSquare"}, {6, "square"},
    };
    auto first = fuzzyMatch("sq", candidates, 10);
    for (int i = 0; i < 5; ++i)
    {
        auto again = fuzzyMatch("sq", candidates, 10);
        REQUIRE(again.size() == first.size());
        for (std::size_t j = 0; j < first.size(); ++j)
        {
            CHECK(again[j].nodeId == first[j].nodeId);
            CHECK(again[j].score == first[j].score);
        }
    }
}

TEST_CASE("fuzzy: limit truncates the (already-sorted) result", "[fuzzy]")
{
    std::vector<std::pair<int64_t, std::string>> candidates{
        {1, "square"}, {2, "squareroot"}, {3, "squarish"}, {4, "esquire"},
    };
    auto hits = fuzzyMatch("squ", candidates, 2);
    CHECK(hits.size() == 2);
}

TEST_CASE("fuzzy: matched indices refer to positions in the ORIGINAL (non-lowercased) text", "[fuzzy]")
{
    std::vector<std::pair<int64_t, std::string>> candidates{{1, "Square"}};
    auto hits = fuzzyMatch("sq", candidates, 10);
    REQUIRE(hits.size() == 1);
    REQUIRE(hits[0].matchedIndices.size() == 2);
    CHECK(hits[0].matchedIndices[0] == 0);
    CHECK(hits[0].matchedIndices[1] == 1);
}
