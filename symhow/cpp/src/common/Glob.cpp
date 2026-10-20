#include "symhow/common/Glob.h"

#include <string_view>
#include <utility>
#include <vector>

namespace symhow::common::glob
{

namespace
{

struct Unit
{
    enum class Kind
    {
        Literal,
        AnyChar,    // '?'
        CharClass,  // '[...]'
        Star,       // '*'  (excludes '/')
        GlobStar,   // '**' (includes '/')
    };

    Kind kind = Kind::Literal;
    char literal = '\0';
    bool negate = false;
    std::vector<std::pair<char, char>> ranges;  // inclusive [lo, hi] pairs, for CharClass
};

bool charClassMatches(const Unit& unit, char c)
{
    bool inSet = false;
    for (const auto& [lo, hi] : unit.ranges)
    {
        if (c >= lo && c <= hi)
        {
            inSet = true;
            break;
        }
    }
    return unit.negate ? !inSet : inSet;
}

bool unitMatchesChar(const Unit& unit, char c)
{
    switch (unit.kind)
    {
        case Unit::Kind::Literal:
            return unit.literal == c;
        case Unit::Kind::AnyChar:
            return true;
        case Unit::Kind::CharClass:
            return charClassMatches(unit, c);
        default:
            return false;  // Star/GlobStar handled separately
    }
}

std::vector<Unit> tokenize(std::string_view pattern)
{
    std::vector<Unit> units;
    std::size_t i = 0;
    const std::size_t n = pattern.size();

    while (i < n)
    {
        const char c = pattern[i];

        if (c == '*')
        {
            if (i + 1 < n && pattern[i + 1] == '*')
            {
                Unit u;
                u.kind = Unit::Kind::GlobStar;
                units.push_back(u);
                i += 2;
                // Collapse any additional consecutive '*' into the same globstar.
                while (i < n && pattern[i] == '*')
                {
                    ++i;
                }
            }
            else
            {
                Unit u;
                u.kind = Unit::Kind::Star;
                units.push_back(u);
                ++i;
            }
            continue;
        }

        if (c == '?')
        {
            Unit u;
            u.kind = Unit::Kind::AnyChar;
            units.push_back(u);
            ++i;
            continue;
        }

        if (c == '[')
        {
            std::size_t j = i + 1;
            bool negate = false;
            if (j < n && (pattern[j] == '!' || pattern[j] == '^'))
            {
                negate = true;
                ++j;
            }
            const std::size_t contentStart = j;
            const std::size_t closeIdx = pattern.find(']', contentStart);
            if (closeIdx == std::string_view::npos)
            {
                // No closing bracket: treat '[' as a literal character.
                Unit u;
                u.kind = Unit::Kind::Literal;
                u.literal = '[';
                units.push_back(u);
                ++i;
                continue;
            }

            Unit u;
            u.kind = Unit::Kind::CharClass;
            u.negate = negate;
            std::string_view content = pattern.substr(contentStart, closeIdx - contentStart);
            for (std::size_t k = 0; k < content.size(); ++k)
            {
                if (k + 2 < content.size() && content[k + 1] == '-')
                {
                    u.ranges.emplace_back(content[k], content[k + 2]);
                    k += 2;
                }
                else
                {
                    u.ranges.emplace_back(content[k], content[k]);
                }
            }
            units.push_back(u);
            i = closeIdx + 1;
            continue;
        }

        Unit u;
        u.kind = Unit::Kind::Literal;
        u.literal = c;
        units.push_back(u);
        ++i;
    }

    return units;
}

// Memoized recursive matcher: matches units[ui..] against str[si..].
// memo[ui][si]: -1 = unknown, 0 = false, 1 = true.
bool matchRec(
    const std::vector<Unit>& units,
    std::size_t ui,
    std::string_view str,
    std::size_t si,
    std::vector<std::vector<signed char>>& memo)
{
    signed char& cached = memo[ui][si];
    if (cached != -1)
    {
        return cached != 0;
    }

    bool result = false;

    if (ui == units.size())
    {
        result = (si == str.size());
    }
    else if (units[ui].kind == Unit::Kind::Star)
    {
        // Zero-length match first, then try consuming one more non-'/' char.
        result = matchRec(units, ui + 1, str, si, memo);
        if (!result && si < str.size() && str[si] != '/')
        {
            result = matchRec(units, ui, str, si + 1, memo);
        }
    }
    else if (units[ui].kind == Unit::Kind::GlobStar)
    {
        // Zero-length match, continuing right after the globstar.
        result = matchRec(units, ui + 1, str, si, memo);

        // Segment-collapse special case: "a/**/b"-style patterns (a
        // globstar immediately followed by a literal '/') should also
        // match zero path segments, i.e. "a/**/b" matches "a/b" too, the
        // same way bash's globstar / minimatch / git pathspecs behave.
        // Without this, matching a zero-length globstar would still
        // require the following literal '/' to be present in the string,
        // which isn't the case when there's no intervening directory at
        // all.
        if (!result && ui + 1 < units.size() && units[ui + 1].kind == Unit::Kind::Literal &&
            units[ui + 1].literal == '/')
        {
            result = matchRec(units, ui + 2, str, si, memo);
        }

        if (!result && si < str.size())
        {
            result = matchRec(units, ui, str, si + 1, memo);
        }
    }
    else
    {
        if (si < str.size() && unitMatchesChar(units[ui], str[si]))
        {
            result = matchRec(units, ui + 1, str, si + 1, memo);
        }
        else
        {
            result = false;
        }
    }

    cached = result ? 1 : 0;
    return result;
}

}  // namespace

bool match(const std::string& pattern, const std::string& path)
{
    const std::vector<Unit> units = tokenize(pattern);
    std::vector<std::vector<signed char>> memo(units.size() + 1, std::vector<signed char>(path.size() + 1, -1));
    return matchRec(units, 0, path, 0, memo);
}

}  // namespace symhow::common::glob
