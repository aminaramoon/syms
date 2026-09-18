// Loads test/integration/data/cucs_cdb_samples.json -- 3 entries copied
// verbatim from the real cucs corpus's compile_commands.json (one .cpp, one
// .cc, one .cu) -- and runs the full CompileDb::load() + ArgSanitizer
// pipeline against them, the same way `kg index --dry-run` does against the
// real 200-entry CDB.

#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <string>

#include "kg/index/ArgSanitizer.h"
#include "kg/index/CompileDb.h"

namespace
{

std::filesystem::path samplesPath()
{
    // This file lives at test/unit/test_cdb_samples.cpp; the fixture lives
    // at test/integration/data/cucs_cdb_samples.json, i.e. ../integration/data
    // relative to this translation unit's source file, or more robustly,
    // resolved from the CMake source dir baked in at configure time.
#ifdef KG_TEST_SOURCE_DIR
    return std::filesystem::path(KG_TEST_SOURCE_DIR) / "integration" / "data" / "cucs_cdb_samples.json";
#else
    return "test/integration/data/cucs_cdb_samples.json";
#endif
}

// Writes `content` to a fresh scratch file under KG_TEST_BINARY_DIR (falling
// back to the system temp dir) and returns its path, for tests that need a
// small ad-hoc compile_commands.json rather than the real cucs fixture.
std::filesystem::path writeScratchCdb(const std::string& name, const std::string& content)
{
#ifdef KG_TEST_BINARY_DIR
    std::filesystem::path dir(KG_TEST_BINARY_DIR);
#else
    std::filesystem::path dir = std::filesystem::temp_directory_path();
#endif
    std::filesystem::create_directories(dir);
    const std::filesystem::path path = dir / name;
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << content;
    out.close();
    return path;
}

}  // namespace

TEST_CASE("cucs CDB samples: full remap + sanitize pipeline", "[cdb][cucs]")
{
    kg::index::CompileDbOptions dbOptions;
    dbOptions.remaps.push_back({"/mnt/workspace", "/home/aaramoon/workspace"});

    const auto entries = kg::index::load(samplesPath().string(), dbOptions);
    REQUIRE(entries.size() == 3);

    kg::index::SanitizerOptions sanitizerOptions;
    sanitizerOptions.resourceDir = "/opt/resource-dir";

    int cuCount = 0;
    for (const auto& entry : entries)
    {
        INFO("file: " << entry.file);

        // No stale /mnt/workspace prefix anywhere, including inside directory/file.
        CHECK(entry.directory.find("/mnt/workspace") == std::string::npos);
        CHECK(entry.file.find("/mnt/workspace") == std::string::npos);

        const auto result = kg::index::sanitize(entry, sanitizerOptions);

        for (const auto& tok : result.args)
        {
            INFO("token: " << tok);
            CHECK(tok.find("/mnt/workspace") == std::string::npos);
        }

        // None of these should ever survive sanitizing.
        for (const auto& tok : result.args)
        {
            CHECK(tok.find("-fmodules-ts") == std::string::npos);
            CHECK(tok.find("-fmodule-mapper") == std::string::npos);
            CHECK(tok.find("-fdeps-format") == std::string::npos);
            CHECK(tok.find("forward-unknown") == std::string::npos);
        }

        if (entry.file.size() >= 3 && entry.file.compare(entry.file.size() - 3, 3, ".cu") == 0)
        {
            ++cuCount;
            bool sawXCuda = false;
            for (std::size_t i = 0; i + 1 < result.args.size(); ++i)
            {
                if (result.args[i] == "-x" && result.args[i + 1] == "cuda")
                {
                    sawXCuda = true;
                    break;
                }
            }
            CHECK(sawXCuda);
        }
    }

    CHECK(cuCount == 1);
}

TEST_CASE("cdb_samples: CompileDb::load supports the already-tokenized 'arguments' array form", "[cdb]")
{
    // The real cucs CDB only uses the `command` string form; the CDB spec
    // also allows an `arguments` array (already split, no shell quoting to
    // undo). Exercise that path directly since nothing else in this test
    // suite does.
    const std::string json = R"([
        {
            "directory": "/work",
            "file": "/work/foo.cpp",
            "arguments": ["/usr/bin/c++", "-DFOO=1", "-I/work/include", "-c", "foo.cpp", "-o", "foo.o"]
        }
    ])";
    const auto path = writeScratchCdb("arguments_form.json", json);

    kg::index::CompileDbOptions dbOptions;
    const auto entries = kg::index::load(path.string(), dbOptions);
    REQUIRE(entries.size() == 1);
    CHECK(entries[0].directory == "/work");
    CHECK(entries[0].file == "/work/foo.cpp");
    CHECK(entries[0].output.empty());
    CHECK(
        entries[0].args ==
        std::vector<std::string>{"/usr/bin/c++", "-DFOO=1", "-I/work/include", "-c", "foo.cpp", "-o", "foo.o"});
}

TEST_CASE("cdb_samples: CompileDb::load applies remap to the output field too", "[cdb]")
{
    const std::string json = R"([
        {
            "directory": "/mnt/workspace/build",
            "file": "/mnt/workspace/src/foo.cpp",
            "output": "/mnt/workspace/build/foo.o",
            "arguments": ["/usr/bin/c++", "-c", "/mnt/workspace/src/foo.cpp"]
        }
    ])";
    const auto path = writeScratchCdb("output_remap.json", json);

    kg::index::CompileDbOptions dbOptions;
    dbOptions.remaps.push_back({"/mnt/workspace", "/home/aaramoon/workspace"});
    const auto entries = kg::index::load(path.string(), dbOptions);
    REQUIRE(entries.size() == 1);
    CHECK(entries[0].output == "/home/aaramoon/workspace/build/foo.o");
}

TEST_CASE("cdb_samples: CompileDb::load --include/--exclude filter on the post-remap file path", "[cdb]")
{
    const std::string json = R"([
        {"directory": "/w", "file": "/mnt/workspace/src/foo.cpp", "arguments": ["/usr/bin/c++", "foo.cpp"]},
        {"directory": "/w", "file": "/mnt/workspace/test/bar.cpp", "arguments": ["/usr/bin/c++", "bar.cpp"]}
    ])";
    const auto path = writeScratchCdb("include_exclude.json", json);

    kg::index::CompileDbOptions dbOptions;
    dbOptions.remaps.push_back({"/mnt/workspace", "/home/aaramoon/workspace"});
    dbOptions.includeGlobs.push_back("/home/aaramoon/workspace/src/**");
    const auto entries = kg::index::load(path.string(), dbOptions);
    REQUIRE(entries.size() == 1);
    CHECK(entries[0].file == "/home/aaramoon/workspace/src/foo.cpp");
}
