#include "kg/query/FuzzyMatcher.h"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <optional>

namespace kg::query
{

namespace
{

char toLowerChar(char c)
{
    return static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
}

std::string toLower(const std::string& s)
{
    std::string out(s.size(), '\0');
    std::transform(s.begin(), s.end(), out.begin(), toLowerChar);
    return out;
}

// Bit `c - 'a'` for lowercase letters; every other character is spread
// across the remaining 38 bits via a cheap hash. False positives (two
// different characters mapping to the same bit) only ever widen the fast
// pre-filter, never narrow it -- the real greedy scan afterwards is what
// determines an actual match, so this stays correct regardless.
std::uint64_t charBit(char c)
{
    if (c >= 'a' && c <= 'z')
    {
        return std::uint64_t{1} << (c - 'a');
    }
    const unsigned char u = static_cast<unsigned char>(c);
    return std::uint64_t{1} << (26 + (u % 38));
}

std::uint64_t buildBitset(const std::string& lower)
{
    std::uint64_t bits = 0;
    for (char c : lower)
    {
        bits |= charBit(c);
    }
    return bits;
}

bool isBoundaryChar(char c)
{
    return c == '.' || c == '_' || c == ':' || c == '/' || c == '\\';
}

struct GreedyMatch
{
    int score = 0;
    std::vector<int> indices;
};

std::optional<GreedyMatch> greedyScore(const std::string& patternLower, const std::string& textLower, const std::string& textOriginal)
{
    if (patternLower.empty())
    {
        return GreedyMatch{};
    }

    GreedyMatch match;
    match.indices.reserve(patternLower.size());

    int cursor = 0;
    int prevIdx = -1;
    int firstIdx = -1;

    for (char pc : patternLower)
    {
        const std::string::size_type found = textLower.find(pc, static_cast<std::string::size_type>(cursor));
        if (found == std::string::npos)
        {
            return std::nullopt;
        }
        const int idx = static_cast<int>(found);

        if (firstIdx < 0)
        {
            firstIdx = idx;
        }
        if (idx == 0)
        {
            match.score += 4;
        }
        if (prevIdx >= 0)
        {
            if (idx == prevIdx + 1)
            {
                match.score += 4;
            }
            else
            {
                const int gap = idx - prevIdx;
                match.score -= (gap - 1);
            }
        }
        if (idx > 0 && isBoundaryChar(textOriginal[static_cast<std::size_t>(idx - 1)]))
        {
            match.score += 4;
        }
        if (idx > 0)
        {
            const unsigned char curOrig = static_cast<unsigned char>(textOriginal[static_cast<std::size_t>(idx)]);
            const unsigned char prevOrig = static_cast<unsigned char>(textOriginal[static_cast<std::size_t>(idx - 1)]);
            if (std::isupper(curOrig) != 0 && std::islower(prevOrig) != 0)
            {
                match.score += 3;
            }
        }

        match.indices.push_back(idx);
        prevIdx = idx;
        cursor = idx + 1;
    }

    if (firstIdx >= 0)
    {
        match.score += std::max(-firstIdx, -20);
    }
    return match;
}

}  // namespace

std::vector<FuzzyHit> fuzzyMatch(const std::string& pattern, const std::vector<std::pair<int64_t, std::string>>& candidates, int limit)
{
    const std::string patternLower = toLower(pattern);
    const std::uint64_t patternBits = buildBitset(patternLower);

    struct Scored
    {
        FuzzyHit hit;
        std::size_t textLength;
    };
    std::vector<Scored> scored;
    scored.reserve(candidates.size());

    for (const auto& [id, text] : candidates)
    {
        const std::string textLower = toLower(text);
        const std::uint64_t textBits = buildBitset(textLower);
        if ((patternBits & textBits) != patternBits)
        {
            continue;
        }

        std::optional<GreedyMatch> m = greedyScore(patternLower, textLower, text);
        if (!m.has_value())
        {
            continue;
        }

        FuzzyHit hit;
        hit.nodeId = id;
        hit.score = m->score;
        hit.matchedIndices = std::move(m->indices);
        scored.push_back(Scored{std::move(hit), text.size()});
    }

    std::sort(scored.begin(), scored.end(), [](const Scored& a, const Scored& b) {
        if (a.hit.score != b.hit.score)
        {
            return a.hit.score > b.hit.score;
        }
        if (a.textLength != b.textLength)
        {
            return a.textLength < b.textLength;
        }
        return a.hit.nodeId < b.hit.nodeId;
    });

    if (limit > 0 && scored.size() > static_cast<std::size_t>(limit))
    {
        scored.resize(static_cast<std::size_t>(limit));
    }

    std::vector<FuzzyHit> out;
    out.reserve(scored.size());
    for (Scored& s : scored)
    {
        out.push_back(std::move(s.hit));
    }
    return out;
}

}  // namespace kg::query
