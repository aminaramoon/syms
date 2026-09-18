#include "kg/index/CompileDb.h"

#include <fstream>
#include <sstream>
#include <stdexcept>

#include <nlohmann/json.hpp>

#include "kg/common/Glob.h"
#include "kg/common/Shell.h"

namespace kg::index
{

std::pair<std::string, std::string> parseRemapArg(const std::string& spec)
{
    const std::size_t eq = spec.find('=');
    if (eq == std::string::npos)
    {
        throw std::invalid_argument("invalid --remap value (expected OLD=NEW): " + spec);
    }
    return {spec.substr(0, eq), spec.substr(eq + 1)};
}

std::string applyRemaps(const std::string& text, const std::vector<std::pair<std::string, std::string>>& remaps)
{
    std::string result = text;
    for (const auto& [oldStr, newStr] : remaps)
    {
        if (oldStr.empty())
        {
            continue;
        }
        std::string next;
        next.reserve(result.size());
        std::size_t pos = 0;
        while (true)
        {
            const std::size_t found = result.find(oldStr, pos);
            if (found == std::string::npos)
            {
                next.append(result, pos, std::string::npos);
                break;
            }
            next.append(result, pos, found - pos);
            next.append(newStr);
            pos = found + oldStr.size();
        }
        result = std::move(next);
    }
    return result;
}

namespace
{

bool matchesAny(const std::vector<std::string>& globs, const std::string& path)
{
    for (const auto& g : globs)
    {
        if (kg::common::glob::match(g, path))
        {
            return true;
        }
    }
    return false;
}

}  // namespace

std::vector<CompileEntry> load(const std::string& path, const CompileDbOptions& options)
{
    std::ifstream in(path, std::ios::binary);
    if (!in)
    {
        throw std::runtime_error("cannot open compile database: " + path);
    }

    std::ostringstream buf;
    buf << in.rdbuf();

    nlohmann::json doc;
    try
    {
        doc = nlohmann::json::parse(buf.str());
    }
    catch (const nlohmann::json::parse_error& e)
    {
        throw std::runtime_error("failed to parse compile database '" + path + "': " + e.what());
    }

    if (!doc.is_array())
    {
        throw std::runtime_error("compile database '" + path + "' is not a JSON array");
    }

    std::vector<CompileEntry> result;
    result.reserve(doc.size());

    for (const auto& entryJson : doc)
    {
        CompileEntry entry;
        entry.directory = entryJson.value("directory", std::string());
        entry.file = entryJson.value("file", std::string());
        entry.output = entryJson.value("output", std::string());

        if (entryJson.contains("arguments") && entryJson.at("arguments").is_array())
        {
            for (const auto& tok : entryJson.at("arguments"))
            {
                entry.args.push_back(tok.get<std::string>());
            }
        }
        else if (entryJson.contains("command") && entryJson.at("command").is_string())
        {
            entry.args = kg::common::shell::split(entryJson.at("command").get<std::string>());
        }
        else
        {
            // Malformed entry (no way to know what to compile with): skip it
            // rather than failing the whole load.
            continue;
        }

        // Step 1: REMAP -- apply to directory, file, output, and every arg token.
        entry.directory = applyRemaps(entry.directory, options.remaps);
        entry.file = applyRemaps(entry.file, options.remaps);
        if (!entry.output.empty())
        {
            entry.output = applyRemaps(entry.output, options.remaps);
        }
        for (auto& tok : entry.args)
        {
            tok = applyRemaps(tok, options.remaps);
        }

        // Include/exclude filtering, matched against the post-remap file path.
        if (!options.includeGlobs.empty() && !matchesAny(options.includeGlobs, entry.file))
        {
            continue;
        }
        if (matchesAny(options.excludeGlobs, entry.file))
        {
            continue;
        }

        result.push_back(std::move(entry));
    }

    return result;
}

}  // namespace kg::index
