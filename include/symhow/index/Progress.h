#pragma once

#include <cstdint>
#include <string>

// Pure formatting helpers for `symhow index`'s stderr progress output (Feature:
// progress display). Kept free of any I/O/threading so the exact text is
// unit-testable (test/unit/test_progress.cpp) independent of the real
// (possibly parallel) indexing pipeline in IndexDriver.cpp.
namespace symhow::index::progress
{

// Right-aligned "[  12/975]" counter: `index` is 1-based, width is derived
// from the decimal digit count of `total` (so every line in a run lines up
// in a fixed-width column regardless of how far along the counter is).
std::string formatCounter(uint32_t index, uint32_t total);

// Per-TU wall time, e.g. "1.3s" / "0.1s" / "42.0s" -- always one decimal.
std::string formatTuSeconds(double seconds);

// A human duration used for both "elapsed" and "eta", e.g. "41s", "9m41s",
// "1h08m20s". Never negative; a negative or non-finite input is clamped to
// "0s".
std::string formatDurationHMS(double seconds);

// Full per-TU progress line (without trailing newline), e.g.
// "[ 12/975] 1.3s  ok        src/foo/bar.cpp" or
// "[ 13/975] 4.1s  ok (cuda rung 1)  src/op/x.cu".
std::string formatProgressLine(uint32_t index, uint32_t total, double seconds, const std::string& status, const std::string& path);

// Periodic/final one-line summary, e.g.
// "symhow index: 120/975 done, 12.3 TU/min, elapsed 9m41s, eta ~68m"
// (or, once `done == total`, no eta: "symhow index: 975/975 done, ...").
std::string formatRunningSummary(uint32_t done, uint32_t total, double tusPerMin, double elapsedSeconds, double etaSeconds);

// Returns `absPath` relative to `repoRoot` (POSIX-style, "/"-joined) if
// `repoRoot` is non-empty and `absPath` lexically falls inside it; else
// returns `absPath` unchanged. Purely lexical/textual (no filesystem
// access), so it works for paths that may not exist on the machine running
// the formatter (e.g. unit tests).
std::string relativeToRoot(const std::string& absPath, const std::string& repoRoot);

}  // namespace symhow::index::progress
