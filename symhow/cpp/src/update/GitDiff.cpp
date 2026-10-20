#include "symhow/update/GitDiff.h"

namespace symhow::update
{

namespace
{

std::string makeAbsolute(const std::filesystem::path& repoRoot, std::string_view relOrAbs)
{
    std::filesystem::path p(relOrAbs);
    if (!p.is_absolute())
    {
        p = repoRoot / p;
    }
    return p.lexically_normal().string();
}

// Splits `s` on NUL bytes, dropping a single trailing empty token (git
// always NUL-*terminates*, not NUL-*separates*, so splitting naively on '\0'
// leaves one bogus empty token at the end -- unless the input is empty, in
// which case there's nothing to drop).
std::vector<std::string_view> splitNul(std::string_view s)
{
    std::vector<std::string_view> tokens;
    std::size_t start = 0;
    while (start <= s.size())
    {
        const std::size_t nul = s.find('\0', start);
        if (nul == std::string_view::npos)
        {
            if (start < s.size())
            {
                tokens.push_back(s.substr(start));
            }
            break;
        }
        tokens.push_back(s.substr(start, nul - start));
        start = nul + 1;
    }
    return tokens;
}

}  // namespace

std::vector<ChangedFile> parseNameStatusZ(std::string_view nulSeparated, const std::filesystem::path& repoRoot)
{
    std::vector<ChangedFile> result;
    const std::vector<std::string_view> tokens = splitNul(nulSeparated);

    std::size_t i = 0;
    while (i < tokens.size())
    {
        const std::string_view status = tokens[i++];
        if (status.empty())
        {
            continue;
        }
        const char code = status[0];

        if (code == 'R' || code == 'C')
        {
            if (i + 1 >= tokens.size())
            {
                break;  // malformed/truncated input -- nothing more to parse
            }
            const std::string oldPath = makeAbsolute(repoRoot, tokens[i++]);
            const std::string newPath = makeAbsolute(repoRoot, tokens[i++]);
            if (code == 'R')
            {
                result.push_back(ChangedFile{ChangeKind::Renamed, oldPath, newPath});
            }
            else
            {
                // Copy: the source is untouched, so treat this like a new
                // TU showing up at `newPath`.
                result.push_back(ChangedFile{ChangeKind::Added, "", newPath});
            }
            continue;
        }

        if (i >= tokens.size())
        {
            break;  // malformed/truncated input
        }
        const std::string path = makeAbsolute(repoRoot, tokens[i++]);

        ChangeKind kind = ChangeKind::Modified;
        switch (code)
        {
            case 'A':
                kind = ChangeKind::Added;
                break;
            case 'D':
                kind = ChangeKind::Deleted;
                break;
            case 'U':
                kind = ChangeKind::Unmerged;
                break;
            case 'M':
            case 'T':
            case 'X':
            case 'B':
            default:
                kind = ChangeKind::Modified;
                break;
        }
        result.push_back(ChangedFile{kind, "", path});
    }

    return result;
}

std::vector<std::string> parseStatusPorcelainZ(std::string_view nulSeparated, const std::filesystem::path& repoRoot)
{
    std::vector<std::string> result;
    const std::vector<std::string_view> tokens = splitNul(nulSeparated);

    std::size_t i = 0;
    while (i < tokens.size())
    {
        const std::string_view entry = tokens[i++];
        if (entry.size() < 3)
        {
            continue;
        }
        const char x = entry[0];
        const char y = entry[1];
        // entry[2] is the space separating "XY" from the path.
        const std::string_view path = entry.substr(3);

        if (x == '!' && y == '!')
        {
            continue;  // ignored -- not a "dirty" entry
        }

        result.push_back(makeAbsolute(repoRoot, path));

        if (x == 'R' || x == 'C' || y == 'R' || y == 'C')
        {
            // Rename/copy: the next token is the old path -- consume it (we
            // only report the current path).
            if (i < tokens.size())
            {
                ++i;
            }
        }
    }

    return result;
}

bool statusHasUnmergedEntries(std::string_view nulSeparated)
{
    const std::vector<std::string_view> tokens = splitNul(nulSeparated);
    std::size_t i = 0;
    while (i < tokens.size())
    {
        const std::string_view entry = tokens[i++];
        if (entry.size() < 2)
        {
            continue;
        }
        const char x = entry[0];
        const char y = entry[1];
        if (x == 'U' || y == 'U')
        {
            return true;
        }
        if (x == 'R' || x == 'C' || y == 'R' || y == 'C')
        {
            // Rename/copy: the next token is the old path -- consume it,
            // same as parseStatusPorcelainZ.
            if (i < tokens.size())
            {
                ++i;
            }
        }
    }
    return false;
}

}  // namespace symhow::update
