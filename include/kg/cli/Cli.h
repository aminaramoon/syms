#pragma once

// kg command-line entry point.
//
// Phase 1 scope: this only implements enough argument handling to support
// `kg --version`, `kg --help`/`-h`, and rejecting unknown flags with a
// non-zero exit code and a helpful message. A real subcommand/argument
// parser (index, query, etc.) is added in later phases; hand-rolled parsing
// is intentional and sufficient for now.

namespace kg::cli
{

// Runs the kg CLI. Returns the process exit code.
int run(int argc, char** argv);

// Prints `kg --version` output to stdout.
void printVersion();

// Prints `kg --help` output to stdout.
void printHelp(const char* programName);

}  // namespace kg::cli
