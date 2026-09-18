#include <catch2/catch_test_macros.hpp>

#include "kg/common/Shell.h"

using kg::common::shell::split;

TEST_CASE("shell_split: shell::split handles plain space-separated tokens", "[shell]")
{
    const auto tokens = split("gcc -c foo.cpp -o foo.o");
    REQUIRE(tokens == std::vector<std::string>{"gcc", "-c", "foo.cpp", "-o", "foo.o"});
}

TEST_CASE("shell_split: shell::split collapses repeated whitespace and trims", "[shell]")
{
    const auto tokens = split("  gcc   -c   foo.cpp  ");
    REQUIRE(tokens == std::vector<std::string>{"gcc", "-c", "foo.cpp"});
}

TEST_CASE("shell_split: shell::split handles single-quoted tokens literally", "[shell]")
{
    const auto tokens = split(R"(gcc -DFOO='bar baz' -c)");
    REQUIRE(tokens == std::vector<std::string>{"gcc", "-DFOO=bar baz", "-c"});
}

TEST_CASE("shell_split: shell::split treats single quotes as fully literal (no escapes)", "[shell]")
{
    const auto tokens = split(R"('a\b')");
    REQUIRE(tokens == std::vector<std::string>{R"(a\b)"});
}

TEST_CASE("shell_split: shell::split handles double-quoted tokens", "[shell]")
{
    const auto tokens = split(R"(gcc -DFOO="bar baz" -c)");
    REQUIRE(tokens == std::vector<std::string>{"gcc", "-DFOO=bar baz", "-c"});
}

TEST_CASE("shell_split: shell::split honors backslash escapes inside double quotes", "[shell]")
{
    const auto tokens = split(R"("a \" b \\ c")");
    REQUIRE(tokens == std::vector<std::string>{R"(a " b \ c)"});
}

TEST_CASE("shell_split: shell::split honors backslash escapes outside quotes", "[shell]")
{
    const auto tokens = split(R"(foo\ bar baz)");
    REQUIRE(tokens == std::vector<std::string>{"foo bar", "baz"});
}

TEST_CASE("shell_split: shell::split concatenates adjacent quoted/unquoted segments into one token", "[shell]")
{
    const auto tokens = split(R"(foo"bar baz"qux)");
    REQUIRE(tokens == std::vector<std::string>{"foobar bazqux"});
}

TEST_CASE("shell_split: shell::split reproduces the real cucs CDB --generate-code token as one token", "[shell][cucs]")
{
    // Verbatim from /home/aaramoon/workspace/oss/cucs/compile_commands.json's
    // sole .cu entry.
    const auto tokens = split(R"("--generate-code=arch=compute_75,code=[sm_75]")");
    REQUIRE(tokens.size() == 1);
    CHECK(tokens[0] == "--generate-code=arch=compute_75,code=[sm_75]");
}

TEST_CASE("shell_split: shell::split handles the full nvcc --generate-code sequence from the real cucs CDB", "[shell][cucs]")
{
    const auto tokens = split(
        R"("--generate-code=arch=compute_75,code=[sm_75]" "--generate-code=arch=compute_80,code=[sm_80]" -Xcompiler=-fPIE -Xcompiler -pthread -x cu)");
    REQUIRE(
        tokens ==
        std::vector<std::string>{
            "--generate-code=arch=compute_75,code=[sm_75]",
            "--generate-code=arch=compute_80,code=[sm_80]",
            "-Xcompiler=-fPIE",
            "-Xcompiler",
            "-pthread",
            "-x",
            "cu"});
}

TEST_CASE("shell_split: shell::split returns an empty vector for an empty/whitespace-only command", "[shell]")
{
    CHECK(split("").empty());
    CHECK(split("   ").empty());
}
