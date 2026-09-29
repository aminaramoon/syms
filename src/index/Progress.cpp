#include "kg/index/Progress.h"

#include <algorithm>
#include <cmath>
#include <cstdio>

namespace kg::index::progress
{

namespace
{

uint32_t digitCount(uint32_t v)
{
    uint32_t n = 1;
    while (v >= 10)
    {
        v /= 10;
        ++n;
    }
    return n;
}

}  // namespace

std::string formatCounter(uint32_t index, uint32_t total)
{
    const uint32_t width = digitCount(total);
    char buf[64];
    std::snprintf(buf, sizeof(buf), "[%*u/%u]", static_cast<int>(width), index, total);
    return buf;
}

std::string formatTuSeconds(double seconds)
{
    if (!(seconds > 0.0))
    {
        seconds = 0.0;
    }
    char buf[32];
    std::snprintf(buf, sizeof(buf), "%.1fs", seconds);
    return buf;
}

std::string formatDurationHMS(double seconds)
{
    if (!std::isfinite(seconds) || seconds < 0.0)
    {
        seconds = 0.0;
    }
    uint64_t total = static_cast<uint64_t>(seconds + 0.5);  // round to nearest second
    const uint64_t h = total / 3600;
    total %= 3600;
    const uint64_t m = total / 60;
    const uint64_t s = total % 60;

    char buf[32];
    if (h > 0)
    {
        std::snprintf(buf, sizeof(buf), "%lluh%02llum%02llus", static_cast<unsigned long long>(h), static_cast<unsigned long long>(m), static_cast<unsigned long long>(s));
    }
    else if (m > 0)
    {
        std::snprintf(buf, sizeof(buf), "%llum%02llus", static_cast<unsigned long long>(m), static_cast<unsigned long long>(s));
    }
    else
    {
        std::snprintf(buf, sizeof(buf), "%llus", static_cast<unsigned long long>(s));
    }
    return buf;
}

std::string formatProgressLine(uint32_t index, uint32_t total, double seconds, const std::string& status, const std::string& path)
{
    std::string line = formatCounter(index, total);
    line += ' ';
    line += formatTuSeconds(seconds);
    line += "  ";
    line += status;
    line += "  ";
    line += path;
    return line;
}

std::string formatRunningSummary(uint32_t done, uint32_t total, double tusPerMin, double elapsedSeconds, double etaSeconds)
{
    char rateBuf[32];
    std::snprintf(rateBuf, sizeof(rateBuf), "%.1f", tusPerMin);

    std::string line = "kg index: ";
    line += std::to_string(done) + "/" + std::to_string(total) + " done, ";
    line += rateBuf;
    line += " TU/min, elapsed ";
    line += formatDurationHMS(elapsedSeconds);
    if (done < total)
    {
        line += ", eta ~" + formatDurationHMS(etaSeconds);
    }
    return line;
}

std::string relativeToRoot(const std::string& absPath, const std::string& repoRoot)
{
    if (repoRoot.empty())
    {
        return absPath;
    }
    std::string root = repoRoot;
    while (!root.empty() && root.back() == '/')
    {
        root.pop_back();
    }
    if (absPath.size() > root.size() + 1 && absPath.compare(0, root.size(), root) == 0 && absPath[root.size()] == '/')
    {
        return absPath.substr(root.size() + 1);
    }
    if (absPath == root)
    {
        return ".";
    }
    return absPath;
}

}  // namespace kg::index::progress
