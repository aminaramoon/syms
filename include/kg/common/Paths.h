#pragma once

#include <string>
#include <unordered_map>

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

// Operation-scoped memoization: resolve each original spelling only once.
// Never retain across updates (deleted/generated files and symlink targets
// can change between runs). Like normalize(), this is for matching only.
// Single-threaded; indexing workers must not share this instance.
class NormalizationCache
{
public:
    const std::string& operator()(const std::string& path);
    std::size_t size() const { return m_paths.size(); }

private:
    std::unordered_map<std::string, std::string> m_paths;
};

}  // namespace kg::common::paths
