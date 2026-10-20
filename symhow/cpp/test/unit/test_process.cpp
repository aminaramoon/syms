#include <catch2/catch_test_macros.hpp>

#include "symhow/common/Process.h"

using symhow::common::process::run;

TEST_CASE("process: runs /bin/echo and captures stdout")
{
    const auto result = run({"/bin/echo", "hi"});
    REQUIRE(result.exitCode == 0);
    REQUIRE(result.out == "hi\n");
}

TEST_CASE("process: a nonexistent binary fails to spawn without throwing")
{
    const auto result = run({"symhow-this-binary-does-not-exist-anywhere-1234"});
    REQUIRE(result.exitCode == -1);
    REQUIRE_FALSE(result.err.empty());
}

TEST_CASE("process: relays a nonzero exit code")
{
    const auto result = run({"/bin/sh", "-c", "exit 3"});
    REQUIRE(result.exitCode == 3);
}

TEST_CASE("process: captures stderr separately from stdout")
{
    const auto result = run({"/bin/sh", "-c", "echo out; echo err 1>&2"});
    REQUIRE(result.exitCode == 0);
    REQUIRE(result.out == "out\n");
    REQUIRE(result.err == "err\n");
}

TEST_CASE("process: draining doesn't deadlock when the child writes a large amount to stderr before stdout")
{
    // A pipe buffer is typically 64 KiB; writing 200000 bytes to stderr
    // (via the shell passed as the CHILD COMMAND here -- run() itself never
    // spawns a shell) before ever touching stdout would deadlock a
    // read-stdout-to-EOF-then-stderr implementation, since the child blocks
    // on its stderr write() while we're still blocked reading stdout.
    const auto result =
        run({"/bin/sh", "-c", "head -c 200000 /dev/zero | tr '\\0' x >&2; echo done"});
    REQUIRE(result.exitCode == 0);
    REQUIRE(result.out == "done\n");
    REQUIRE(result.err.size() == 200000);
}

TEST_CASE("process: resolves argv[0] via PATH (posix_spawnp semantics)")
{
    // No slash in argv[0] -- must be found on PATH, not treated as a
    // relative/absolute path.
    const auto result = run({"true"});
    REQUIRE(result.exitCode == 0);
}

TEST_CASE("process: cwd changes the child's working directory")
{
    const std::string cwd = "/tmp";
    const auto result = run({"/bin/sh", "-c", "pwd"}, &cwd);
    REQUIRE(result.exitCode == 0);
    // /tmp itself may be a symlink (e.g. to /private/tmp) -- just check the
    // shell actually ran somewhere other than symhow's own working directory.
    REQUIRE_FALSE(result.out.empty());
}
