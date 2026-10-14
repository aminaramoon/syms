#include "symhow/common/Shell.h"

#include <cctype>

namespace symhow::common::shell
{

namespace
{

bool isEscapableInDoubleQuotes(char c)
{
    return c == '"' || c == '\\' || c == '$' || c == '`';
}

}  // namespace

std::vector<std::string> split(const std::string& command)
{
    std::vector<std::string> tokens;
    std::string current;
    bool inToken = false;

    const std::size_t n = command.size();
    std::size_t i = 0;

    while (i < n)
    {
        const char c = command[i];

        if (std::isspace(static_cast<unsigned char>(c)) != 0)
        {
            if (inToken)
            {
                tokens.push_back(current);
                current.clear();
                inToken = false;
            }
            ++i;
            continue;
        }

        inToken = true;

        if (c == '\'')
        {
            ++i;
            while (i < n && command[i] != '\'')
            {
                current += command[i];
                ++i;
            }
            if (i < n)
            {
                ++i;  // skip closing quote
            }
        }
        else if (c == '"')
        {
            ++i;
            while (i < n && command[i] != '"')
            {
                if (command[i] == '\\' && i + 1 < n && isEscapableInDoubleQuotes(command[i + 1]))
                {
                    current += command[i + 1];
                    i += 2;
                }
                else
                {
                    current += command[i];
                    ++i;
                }
            }
            if (i < n)
            {
                ++i;  // skip closing quote
            }
        }
        else if (c == '\\')
        {
            if (i + 1 < n)
            {
                current += command[i + 1];
                i += 2;
            }
            else
            {
                // Trailing, unmatched backslash: drop it.
                ++i;
            }
        }
        else
        {
            current += c;
            ++i;
        }
    }

    if (inToken)
    {
        tokens.push_back(current);
    }

    return tokens;
}

}  // namespace symhow::common::shell
