// Pure unit tests for kg::index::progress's formatting helpers (Feature:
// `kg index` progress display) -- counter width, duration/eta formatting,
// and the full per-TU/summary line assembly. No fixture DB needed: these
// functions do no I/O.

#include <catch2/catch_test_macros.hpp>

#include "kg/index/Progress.h"

using namespace kg::index::progress;

TEST_CASE("progress: formatCounter right-aligns to total's digit width", "[progress]")
{
    CHECK(formatCounter(12, 975) == "[ 12/975]");
    CHECK(formatCounter(1, 975) == "[  1/975]");
    CHECK(formatCounter(975, 975) == "[975/975]");
    CHECK(formatCounter(1, 9) == "[1/9]");
    CHECK(formatCounter(1, 10) == "[ 1/10]");
    CHECK(formatCounter(5, 1) == "[5/1]");  // width from total's OWN digit count, never index's
}

TEST_CASE("progress: formatTuSeconds always shows one decimal", "[progress]")
{
    CHECK(formatTuSeconds(1.3) == "1.3s");
    CHECK(formatTuSeconds(0.04) == "0.0s");
    CHECK(formatTuSeconds(4.1) == "4.1s");
    CHECK(formatTuSeconds(-1.0) == "0.0s");  // clamped, never negative
}

TEST_CASE("progress: formatDurationHMS scales units and zero-pads", "[progress]")
{
    CHECK(formatDurationHMS(0.0) == "0s");
    CHECK(formatDurationHMS(41.0) == "41s");
    CHECK(formatDurationHMS(59.6) == "1m00s");  // rounds to nearest second
    CHECK(formatDurationHMS(9 * 60 + 41.0) == "9m41s");
    CHECK(formatDurationHMS(60 * 60 + 8 * 60 + 20.0) == "1h08m20s");
    CHECK(formatDurationHMS(-5.0) == "0s");  // clamped
}

TEST_CASE("progress: formatProgressLine assembles counter, time, status, path in order", "[progress]")
{
    const std::string line = formatProgressLine(12, 975, 1.3, "ok", "src/foo/bar.cpp");
    CHECK(line == "[ 12/975] 1.3s  ok  src/foo/bar.cpp");
}

TEST_CASE("progress: formatProgressLine carries a CUDA rung suffix through unchanged", "[progress]")
{
    const std::string line = formatProgressLine(13, 975, 4.1, "ok (cuda rung 1)", "src/op/x.cu");
    CHECK(line == "[ 13/975] 4.1s  ok (cuda rung 1)  src/op/x.cu");
}

TEST_CASE("progress: formatRunningSummary includes an eta while in progress, omits it once done", "[progress]")
{
    const std::string mid = formatRunningSummary(120, 975, 12.3, 9 * 60 + 41.0, 42 * 60.0);
    CHECK(mid == "kg index: 120/975 done, 12.3 TU/min, elapsed 9m41s, eta ~42m00s");

    const std::string done = formatRunningSummary(975, 975, 12.3, 9 * 60 + 41.0, 0.0);
    CHECK(done == "kg index: 975/975 done, 12.3 TU/min, elapsed 9m41s");
    CHECK(done.find("eta") == std::string::npos);
}

TEST_CASE("progress: relativeToRoot strips the repo root and normalizes a bare match to '.'", "[progress]")
{
    CHECK(relativeToRoot("/repo/src/foo.cpp", "/repo") == "src/foo.cpp");
    CHECK(relativeToRoot("/repo/src/foo.cpp", "/repo/") == "src/foo.cpp");  // trailing slash tolerated
    CHECK(relativeToRoot("/repo", "/repo") == ".");
    CHECK(relativeToRoot("/other/src/foo.cpp", "/repo") == "/other/src/foo.cpp");  // outside root: unchanged
    CHECK(relativeToRoot("/repo/src/foo.cpp", "") == "/repo/src/foo.cpp");         // no known root: unchanged
    CHECK(relativeToRoot("/repofoo/x.cpp", "/repo") == "/repofoo/x.cpp");          // no false-positive prefix match
}
