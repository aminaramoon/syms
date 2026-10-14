#pragma once

#include <string>

// Phase 1 placeholder logging facade.
//
// Deliberately minimal: a single free function that writes a line to
// stderr. Later phases may replace this with real leveled/structured
// logging; for now it exists so other modules (and the CLI) have a stable
// header to depend on, and so symhow_common has at least one real translation
// unit.

namespace symhow::common
{

void logInfo(const std::string& message);

}  // namespace symhow::common
