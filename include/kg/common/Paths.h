#pragma once

#include <string>

// Shared path-normalization helper. Lives in kg_common (rather than
// kg_update, where it originated) so kg_index can use it too without
// creating a kg_index -> kg_update dependency cycle (kg_update already
// depends on kg_index; see src/CMakeLists.txt).
namespace kg::common::paths
{

// Normalizes `path` for matching purposes ONLY (never for storage/output):
// makes it absolute, then `weakly_canonical()`s it if it exists on disk
// (resolving symlinks/`.`/`..` so e.g. a symlinked source tree and its
// target agree), else falls back to `lexically_normal()` (purely textual,
// for paths that don't exist on disk -- e.g. a file reported as freshly
// deleted, or a CDB entry for a generated file that hasn't been produced
// yet).
std::string normalize(const std::string& path);

}  // namespace kg::common::paths
