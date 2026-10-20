#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

// Clean-room minimal fuzzy matcher: our own scoring design, inspired by the
// general, publicly-known technique used by fzf/Sublime-style matchers
// (greedy in-order subsequence match + positional bonuses), not a port of
// any specific implementation.
namespace symhow::query
{

struct FuzzyHit
{
    int64_t nodeId = 0;
    int score = 0;
    std::vector<int> matchedIndices;  // into the ORIGINAL (non-lowercased) candidate text
};

// Scores every candidate whose lowercased text contains `pattern`'s
// (lowercased) characters as an in-order subsequence, using a greedy
// leftmost match plus these bonuses (applied while walking the pattern
// left-to-right against the matched positions):
//   +4  the matched index is 0
//   +4  the matched index is exactly one past the previous matched index
//       (a contiguous run) -- mutually exclusive with the next bonus
//   -(gap-1), where gap = matchedIndex - previousMatchedIndex, otherwise
//   +4  the character immediately before the matched index is one of
//       ". _ : / \" (a natural word/segment boundary)
//   +3  the matched character is uppercase in the ORIGINAL (non-lowercased)
//       text and the character before it is lowercase (a camelCase boundary)
// plus a ONE-TIME overall penalty of max(-firstMatchIndex, -20), applied
// once per candidate (not per matched character) after the scan.
//
// A cheap 64-bit "lowercase alphabet bitset" per candidate (lowercase a-z in
// bits 0-25, every other character hashed into bits 26-63) rejects
// candidates that can't possibly match before running the real greedy scan.
//
// Results are sorted descending by score, ties broken by (shorter candidate
// text first, then lower nodeId) for fully deterministic output, then
// truncated to `limit` (no truncation if limit <= 0).
std::vector<FuzzyHit> fuzzyMatch(const std::string& pattern, const std::vector<std::pair<int64_t, std::string>>& candidates, int limit);

}  // namespace symhow::query
