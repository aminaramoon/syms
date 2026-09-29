#include "kg/update/Git.h"

#include <filesystem>
#include <set>

#include "kg/common/Process.h"

namespace kg::update
{

namespace
{

std::string trimTrailingNewline(std::string s)
{
    while (!s.empty() && (s.back() == '\n' || s.back() == '\r'))
    {
        s.pop_back();
    }
    return s;
}

}  // namespace

std::optional<std::string> revParse(const std::string& repoRoot, const std::string& rev)
{
    const kg::common::process::Result result =
        kg::common::process::run({"git", "-C", repoRoot, "rev-parse", "--verify", rev + "^{commit}"});
    if (result.exitCode != 0)
    {
        return std::nullopt;
    }
    return trimTrailingNewline(result.out);
}

std::optional<std::string> topLevel(const std::string& anyDir)
{
    const kg::common::process::Result result = kg::common::process::run({"git", "-C", anyDir, "rev-parse", "--show-toplevel"});
    if (result.exitCode != 0)
    {
        return std::nullopt;
    }
    return std::filesystem::path(trimTrailingNewline(result.out)).lexically_normal().string();
}

std::optional<std::string> diffNameStatusZ(
    const std::string& repoRoot, const std::string& baseline, const std::optional<std::string>& to)
{
    std::vector<std::string> argv = {"git", "-C", repoRoot, "diff", "--name-status", "-z", "--find-renames"};
    if (to.has_value())
    {
        argv.push_back(baseline + ".." + *to);
    }
    else
    {
        argv.push_back(baseline);
    }
    const kg::common::process::Result result = kg::common::process::run(argv);
    if (result.exitCode != 0)
    {
        return std::nullopt;
    }
    return result.out;
}

std::optional<std::string> statusPorcelainZ(const std::string& repoRoot)
{
    const kg::common::process::Result result =
        kg::common::process::run({"git", "-C", repoRoot, "status", "--porcelain", "-z", "--untracked-files=all"});
    if (result.exitCode != 0)
    {
        return std::nullopt;
    }
    return result.out;
}

std::vector<std::string> submodulePaths(const std::string& repoRoot)
{
    std::vector<std::string> paths;
    const kg::common::process::Result result = kg::common::process::run(
        {"git", "-C", repoRoot, "config", "--file", ".gitmodules", "--get-regexp", "^submodule\\..*\\.path$"});
    if (result.exitCode != 0)  // exit 1 = no .gitmodules / no matches
    {
        return paths;
    }
    std::size_t start = 0;
    while (start < result.out.size())
    {
        std::size_t end = result.out.find('\n', start);
        if (end == std::string::npos)
        {
            end = result.out.size();
        }
        const std::string line = result.out.substr(start, end - start);
        start = end + 1;
        // "submodule.<name>.path <value>" -- the value may contain spaces.
        const std::size_t sep = line.find(' ');
        if (sep == std::string::npos || sep + 1 >= line.size())
        {
            continue;
        }
        const std::filesystem::path abs = (std::filesystem::path(repoRoot) / line.substr(sep + 1)).lexically_normal();
        paths.push_back(abs.string());
    }
    return paths;
}

std::vector<std::string> submoduleExcludeGlobs(const std::string& repoRoot)
{
    std::vector<std::string> globs;
    for (const std::string& p : submodulePaths(repoRoot))
    {
        globs.push_back(p + "/**");
    }
    return globs;
}

std::optional<ChangeSet> collectChanges(
    const std::string& repoRoot, const std::string& baseline, const std::optional<std::string>& to, std::string* err)
{
    const std::optional<std::string> baselineSha = revParse(repoRoot, baseline);
    if (!baselineSha.has_value())
    {
        if (err != nullptr)
        {
            *err = "couldn't resolve baseline rev '" + baseline + "' in repo '" + repoRoot + "'";
        }
        return std::nullopt;
    }
    if (to.has_value())
    {
        const std::optional<std::string> toSha = revParse(repoRoot, *to);
        if (!toSha.has_value())
        {
            if (err != nullptr)
            {
                *err = "couldn't resolve 'to' rev '" + *to + "' in repo '" + repoRoot + "'";
            }
            return std::nullopt;
        }
    }

    const std::optional<std::string> diffOut = diffNameStatusZ(repoRoot, baseline, to);
    if (!diffOut.has_value())
    {
        if (err != nullptr)
        {
            *err = "'git diff --name-status' failed in repo '" + repoRoot + "'";
        }
        return std::nullopt;
    }

    ChangeSet changeSet;
    changeSet.changes = parseNameStatusZ(*diffOut, repoRoot);

    for (const ChangedFile& c : changeSet.changes)
    {
        if (c.kind == ChangeKind::Unmerged)
        {
            if (err != nullptr)
            {
                *err = "repository has unmerged paths; resolve conflicts first ('" + c.newPath + "' is unmerged)";
            }
            return std::nullopt;
        }
    }

    if (!to.has_value())
    {
        const std::optional<std::string> statusOut = statusPorcelainZ(repoRoot);
        if (!statusOut.has_value())
        {
            if (err != nullptr)
            {
                *err = "'git status --porcelain' failed in repo '" + repoRoot + "'";
            }
            return std::nullopt;
        }

        if (statusHasUnmergedEntries(*statusOut))
        {
            if (err != nullptr)
            {
                *err = "repository has unmerged paths; resolve conflicts first";
            }
            return std::nullopt;
        }

        changeSet.dirtyPaths = parseStatusPorcelainZ(*statusOut, repoRoot);

        // Dedup untracked-as-Added entries against whatever `git diff`
        // already reported (paths already present as newPath in `changes`,
        // from any kind).
        std::set<std::string> alreadyChanged;
        for (const ChangedFile& c : changeSet.changes)
        {
            if (!c.newPath.empty())
            {
                alreadyChanged.insert(c.newPath);
            }
            if (!c.oldPath.empty())
            {
                alreadyChanged.insert(c.oldPath);
            }
        }

        // `git status --porcelain` entries starting with "??" are
        // untracked -- re-derive that directly from raw output rather than
        // re-parsing, since parseStatusPorcelainZ() intentionally throws
        // away the XY status codes (all it exposes is the path list).
        std::size_t start = 0;
        const std::string& raw = *statusOut;
        while (start < raw.size())
        {
            const std::size_t nul = raw.find('\0', start);
            const std::string entry = raw.substr(start, nul == std::string::npos ? std::string::npos : nul - start);
            const std::size_t next = (nul == std::string::npos) ? raw.size() : nul + 1;
            if (entry.size() > 3 && entry[0] == '?' && entry[1] == '?')
            {
                const std::string path = std::filesystem::path(std::filesystem::path(repoRoot) / entry.substr(3)).lexically_normal().string();
                if (alreadyChanged.insert(path).second)
                {
                    changeSet.changes.push_back(ChangedFile{ChangeKind::Added, "", path});
                }
            }
            start = next;
        }
    }

    return changeSet;
}

}  // namespace kg::update
