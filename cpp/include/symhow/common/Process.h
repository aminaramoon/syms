#pragma once

#include <string>
#include <vector>

// Minimal subprocess runner used wherever symhow needs to shell out (currently:
// `git` for provenance metadata). Deliberately never goes through a shell
// (posix_spawnp with an explicit argv) so callers don't need to worry about
// quoting/injection.
namespace symhow::common::process
{

struct Result
{
    int exitCode = -1;
    std::string out;
    std::string err;
};

// Runs argv[0] with the remaining elements as its arguments (argv[0] is
// looked up on PATH via posix_spawnp's semantics). If `cwd` is non-null, the
// child process chdir()s there before exec. Captures stdout/stderr fully.
//
// If the binary can't be found/spawned at all, returns exitCode == -1 with
// `err` set to a description of the failure (never throws).
Result run(const std::vector<std::string>& argv, const std::string* cwd = nullptr);

}  // namespace symhow::common::process
