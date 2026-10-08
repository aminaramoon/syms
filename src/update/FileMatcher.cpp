#include "kg/update/FileMatcher.h"

#include <unordered_map>

#include "kg/common/Paths.h"

namespace kg::update
{

std::string normalizePath(const std::string& path)
{
    return kg::common::paths::normalize(path);
}

namespace
{

// normalized -> original spelling. Later entries win on collision (matches
// "first-write-wins is arbitrary, don't rely on it" -- collisions here mean
// two DB rows / two CDB entries normalize to the same file, which shouldn't
// happen in practice).
std::unordered_map<std::string, std::string> buildNormalizedIndex(
    const std::unordered_set<std::string>& originals, kg::common::paths::NormalizationCache& normalizePath)
{
    std::unordered_map<std::string, std::string> index;
    for (const std::string& original : originals)
    {
        index[normalizePath(original)] = original;
    }
    return index;
}

std::unordered_map<std::string, std::string> buildNormalizedCdbIndex(
    const std::vector<kg::index::CompileEntry>& cdb, kg::common::paths::NormalizationCache& normalizePath)
{
    std::unordered_map<std::string, std::string> index;
    for (const kg::index::CompileEntry& entry : cdb)
    {
        index[normalizePath(entry.file)] = entry.file;
    }
    return index;
}

}  // namespace

MatchResult matchChanged(const MatchInput& input)
{
    kg::common::paths::NormalizationCache normalizePath;
    return matchChanged(input, normalizePath);
}

MatchResult matchChanged(const MatchInput& input, kg::common::paths::NormalizationCache& normalizePath)
{
    MatchResult result;

    const std::unordered_map<std::string, std::string> dbIndex = buildNormalizedIndex(input.dbFilePaths, normalizePath);
    const std::unordered_map<std::string, std::string> cdbIndex = buildNormalizedCdbIndex(input.cdb, normalizePath);

    for (const ChangedFile& change : input.changes)
    {
        if (change.kind == ChangeKind::Deleted || change.kind == ChangeKind::Renamed)
        {
            const std::string& removedPath = (change.kind == ChangeKind::Deleted) ? change.newPath : change.oldPath;
            const std::string norm = normalizePath(removedPath);
            const auto it = dbIndex.find(norm);
            if (it != dbIndex.end())
            {
                result.deleted.push_back(it->second);
            }
            // else: nothing to clear -- ignore.
        }

        if (change.kind == ChangeKind::Added || change.kind == ChangeKind::Modified || change.kind == ChangeKind::Renamed)
        {
            const std::string norm = normalizePath(change.newPath);
            const auto dbIt = dbIndex.find(norm);
            if (dbIt != dbIndex.end())
            {
                result.changedInDb.push_back(dbIt->second);
                continue;
            }
            const auto cdbIt = cdbIndex.find(norm);
            if (cdbIt != cdbIndex.end())
            {
                result.newTus.push_back(cdbIt->second);
                continue;
            }
            result.unmatched.push_back(change.newPath);
        }
    }

    return result;
}

std::vector<kg::index::CompileEntry> selectEntries(
    const std::vector<kg::index::CompileEntry>& entries, const std::unordered_set<std::string>& normalizedPaths)
{
    kg::common::paths::NormalizationCache normalizePath;
    return selectEntries(entries, normalizedPaths, normalizePath);
}

std::vector<kg::index::CompileEntry> selectEntries(
    const std::vector<kg::index::CompileEntry>& entries, const std::unordered_set<std::string>& normalizedPaths,
    kg::common::paths::NormalizationCache& normalizePath)
{
    std::vector<kg::index::CompileEntry> selected;
    for (const kg::index::CompileEntry& entry : entries)
    {
        if (normalizedPaths.count(normalizePath(entry.file)) != 0)
        {
            selected.push_back(entry);
        }
    }
    return selected;
}

}  // namespace kg::update
