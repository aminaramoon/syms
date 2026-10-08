#pragma once

#include <optional>
#include <string>
#include <string_view>

namespace kg::common
{

// SHA-256 of the exact bytes, lowercase hex. No newline/whitespace normalization.
std::string contentHash(std::string_view bytes);

// Streams a regular file; missing/unreadable/non-regular paths return nullopt.
// Never use timestamps alone to decide whether indexed content is unchanged.
std::optional<std::string> fileContentHash(const std::string& path);

}  // namespace kg::common
