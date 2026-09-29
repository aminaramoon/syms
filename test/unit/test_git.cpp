#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <unistd.h>

#include "kg/common/Process.h"
#include "kg/update/Git.h"

#ifndef KG_TEST_BINARY_DIR
#define KG_TEST_BINARY_DIR "."
#endif

using kg::update::ChangeKind;
using kg::update::collectChanges;

namespace
{

bool gitAvailable()
{
    static const bool available = [] {
        const auto result = kg::common::process::run({"git", "--version"});
        return result.exitCode == 0;
    }();
    return available;
}

void run(const std::vector<std::string>& argv, const std::string& cwd)
{
    const auto result = kg::common::process::run(argv, &cwd);
    REQUIRE(result.exitCode == 0);
}

void writeFile(const std::filesystem::path& p, const std::string& contents)
{
    std::ofstream f(p);
    f << contents;
}

// Fresh git repo under the build dir, unique per test run (REQUIRE below
// isn't reliable enough to guarantee cleanup between runs, and a stale repo
// from a previous run would make these tests non-hermetic).
std::filesystem::path freshRepoDir()
{
    static int counter = 0;
    const std::filesystem::path dir =
        std::filesystem::path(KG_TEST_BINARY_DIR) / ("kg_test_git_repo_" + std::to_string(::getpid()) + "_" + std::to_string(counter++));
    std::filesystem::remove_all(dir);
    std::filesystem::create_directories(dir);
    return dir;
}

bool hasChange(const std::vector<kg::update::ChangedFile>& changes, ChangeKind kind, const std::string& newPathSuffix)
{
    return std::any_of(changes.begin(), changes.end(), [&](const kg::update::ChangedFile& c) {
        return c.kind == kind && c.newPath.size() >= newPathSuffix.size() &&
               c.newPath.compare(c.newPath.size() - newPathSuffix.size(), newPathSuffix.size(), newPathSuffix) == 0;
    });
}

}  // namespace

TEST_CASE("git: collectChanges against the working tree finds a Modified + an Added (untracked) file, and lists both as dirty")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }

    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());
    run({"git", "config", "user.email", "kg-test@example.com"}, repo.string());
    run({"git", "config", "user.name", "kg-test"}, repo.string());

    writeFile(repo / "a.cpp", "int a();\n");
    writeFile(repo / "b.cpp", "int b();\n");
    run({"git", "add", "-A"}, repo.string());
    run({"git", "commit", "-q", "-m", "init"}, repo.string());

    // Modify a tracked file, add an untracked one.
    writeFile(repo / "a.cpp", "int a(); // changed\n");
    writeFile(repo / "c.cpp", "int c();\n");

    std::string err;
    const auto changeSet = collectChanges(repo.string(), "HEAD", std::nullopt, &err);
    REQUIRE(changeSet.has_value());
    REQUIRE(err.empty());

    REQUIRE(hasChange(changeSet->changes, ChangeKind::Modified, "a.cpp"));
    REQUIRE(hasChange(changeSet->changes, ChangeKind::Added, "c.cpp"));
    REQUIRE_FALSE(hasChange(changeSet->changes, ChangeKind::Modified, "b.cpp"));

    REQUIRE(changeSet->dirtyPaths.size() == 2);
    REQUIRE(std::any_of(changeSet->dirtyPaths.begin(), changeSet->dirtyPaths.end(), [](const std::string& p) {
        return p.size() >= 5 && p.compare(p.size() - 5, 5, "a.cpp") == 0;
    }));
    REQUIRE(std::any_of(changeSet->dirtyPaths.begin(), changeSet->dirtyPaths.end(), [](const std::string& p) {
        return p.size() >= 5 && p.compare(p.size() - 5, 5, "c.cpp") == 0;
    }));

    std::filesystem::remove_all(repo);
}

TEST_CASE("git: collectChanges between two fixed commits reports the right set and leaves dirtyPaths empty")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }

    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());
    run({"git", "config", "user.email", "kg-test@example.com"}, repo.string());
    run({"git", "config", "user.name", "kg-test"}, repo.string());

    writeFile(repo / "a.cpp", "int a();\n");
    run({"git", "add", "-A"}, repo.string());
    run({"git", "commit", "-q", "-m", "first"}, repo.string());
    const auto firstSha = kg::update::revParse(repo.string(), "HEAD");
    REQUIRE(firstSha.has_value());

    writeFile(repo / "a.cpp", "int a(); // changed\n");
    writeFile(repo / "d.cpp", "int d();\n");
    run({"git", "add", "-A"}, repo.string());
    run({"git", "commit", "-q", "-m", "second"}, repo.string());
    const auto secondSha = kg::update::revParse(repo.string(), "HEAD");
    REQUIRE(secondSha.has_value());

    std::string err;
    const auto changeSet = collectChanges(repo.string(), *firstSha, secondSha, &err);
    REQUIRE(changeSet.has_value());
    REQUIRE(err.empty());
    REQUIRE(changeSet->dirtyPaths.empty());
    REQUIRE(hasChange(changeSet->changes, ChangeKind::Modified, "a.cpp"));
    REQUIRE(hasChange(changeSet->changes, ChangeKind::Added, "d.cpp"));

    std::filesystem::remove_all(repo);
}

TEST_CASE("git: collectChanges fails cleanly on a bad baseline rev")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }

    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());
    run({"git", "config", "user.email", "kg-test@example.com"}, repo.string());
    run({"git", "config", "user.name", "kg-test"}, repo.string());
    writeFile(repo / "a.cpp", "int a();\n");
    run({"git", "add", "-A"}, repo.string());
    run({"git", "commit", "-q", "-m", "init"}, repo.string());

    std::string err;
    const auto changeSet = collectChanges(repo.string(), "not-a-real-rev", std::nullopt, &err);
    REQUIRE_FALSE(changeSet.has_value());
    REQUIRE_FALSE(err.empty());

    std::filesystem::remove_all(repo);
}

TEST_CASE("git: collectChanges refuses a repo with unmerged (conflicted) paths")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }

    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());
    run({"git", "config", "user.email", "kg-test@example.com"}, repo.string());
    run({"git", "config", "user.name", "kg-test"}, repo.string());

    writeFile(repo / "f.txt", "base\n");
    run({"git", "add", "-A"}, repo.string());
    run({"git", "commit", "-q", "-m", "base"}, repo.string());
    const auto baseSha = kg::update::revParse(repo.string(), "HEAD");
    REQUIRE(baseSha.has_value());

    run({"git", "checkout", "-q", "-b", "b1"}, repo.string());
    writeFile(repo / "f.txt", "b1\n");
    run({"git", "commit", "-q", "-am", "b1"}, repo.string());

    run({"git", "checkout", "-q", "-"}, repo.string());
    writeFile(repo / "f.txt", "main\n");
    run({"git", "commit", "-q", "-am", "main"}, repo.string());

    // Intentionally not checking the exit code -- a merge conflict makes
    // `git merge` exit non-zero on purpose.
    const std::string repoStr = repo.string();
    kg::common::process::run({"git", "merge", "-q", "b1"}, &repoStr);

    std::string err;
    const auto changeSet = collectChanges(repoStr, *baseSha, std::nullopt, &err);
    REQUIRE_FALSE(changeSet.has_value());
    REQUIRE_FALSE(err.empty());

    // Clean up the conflicted merge state so freshRepoDir()'s eventual
    // remove_all() doesn't matter either way, but abort explicitly for
    // clarity.
    kg::common::process::run({"git", "merge", "--abort"}, &repoStr);
    std::filesystem::remove_all(repo);
}

TEST_CASE("git: submoduleExcludeGlobs reads .gitmodules without initialized submodules")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }
    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());

    REQUIRE(kg::update::submodulePaths(repo.string()).empty());

    writeFile(
        repo / ".gitmodules",
        "[submodule \"duckdb\"]\n\tpath = duckdb\n\turl = https://example.invalid/duckdb.git\n"
        "[submodule \"nested\"]\n\tpath = experimental/deps/nested lib\n\turl = https://example.invalid/nested.git\n");

    const std::vector<std::string> globs = kg::update::submoduleExcludeGlobs(repo.string());
    REQUIRE(globs.size() == 2);
    const std::string root = std::filesystem::path(repo).lexically_normal().string();
    CHECK(globs[0] == root + "/duckdb/**");
    CHECK(globs[1] == root + "/experimental/deps/nested lib/**");
}

TEST_CASE("git: topLevel resolves the repo root")
{
    if (!gitAvailable())
    {
        SKIP("git not found on PATH");
    }

    const std::filesystem::path repo = freshRepoDir();
    run({"git", "init", "-q"}, repo.string());
    const auto top = kg::update::topLevel(repo.string());
    REQUIRE(top.has_value());
    REQUIRE(std::filesystem::weakly_canonical(*top) == std::filesystem::weakly_canonical(repo));

    std::filesystem::remove_all(repo);
}
