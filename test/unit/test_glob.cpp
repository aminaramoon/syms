#include <catch2/catch_test_macros.hpp>

#include "kg/common/Glob.h"

using kg::common::glob::match;

TEST_CASE("glob::match matches literal strings exactly", "[glob]")
{
    CHECK(match("foo.cpp", "foo.cpp"));
    CHECK_FALSE(match("foo.cpp", "foo.cc"));
}

TEST_CASE("glob::match '*' matches any run of chars excluding '/'", "[glob]")
{
    CHECK(match("*.cpp", "foo.cpp"));
    CHECK(match("src/*.cpp", "src/foo.cpp"));
    CHECK_FALSE(match("src/*.cpp", "src/nested/foo.cpp"));
    CHECK(match("*", ""));
}

TEST_CASE("glob::match '**' matches any run of chars including '/'", "[glob]")
{
    CHECK(match("src/**/*.cpp", "src/a/b/c/foo.cpp"));
    CHECK(match("src/**/*.cpp", "src/foo.cpp"));
    CHECK(match("**/*.cu", "/home/aaramoon/workspace/oss/cucs/test/memory/test_gpu_kernels.cu"));
    CHECK_FALSE(match("**/*.cu", "/home/aaramoon/workspace/oss/cucs/test/memory/test_gpu_kernels.cpp"));
}

TEST_CASE("glob::match '?' matches exactly one char", "[glob]")
{
    CHECK(match("foo?.cpp", "food.cpp"));
    CHECK_FALSE(match("foo?.cpp", "foo.cpp"));
    CHECK_FALSE(match("foo?.cpp", "fooxy.cpp"));
}

TEST_CASE("glob::match '[...]' character classes", "[glob]")
{
    CHECK(match("foo[0-9].cpp", "foo3.cpp"));
    CHECK_FALSE(match("foo[0-9].cpp", "fooa.cpp"));
    CHECK(match("foo[!0-9].cpp", "fooa.cpp"));
    CHECK_FALSE(match("foo[!0-9].cpp", "foo3.cpp"));
    CHECK(match("foo[abc].cpp", "fooa.cpp"));
}

TEST_CASE("glob::match against the real cucs benchmark-src exclude case", "[glob][cucs]")
{
    CHECK(match("**/_deps/**", "/home/aaramoon/workspace/oss/cucs/build/release/_deps/benchmark-src/src/benchmark.cc"));
    CHECK_FALSE(match("**/_deps/**", "/home/aaramoon/workspace/oss/cucs/src/memory/topology_discovery.cpp"));
}
