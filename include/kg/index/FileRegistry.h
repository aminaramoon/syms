#pragma once

#include <cstdint>
#include <string>
#include <unordered_map>
#include <vector>

#include <clang-c/Index.h>

// Per-TU cache of file identity/status (system-header-ness, whether it's
// "indexed" per --include/--exclude, and whether it's a direct #include of
// an indexed file), plus the --system-file-rows policy that decides which
// non-indexed files still get a FileFact row. Keyed by CXFileUniqueID so
// distinct CXFile handles that alias the same on-disk file (common via
// different #include search-path spellings) still share one Info.
namespace kg::index
{

enum class SystemFileRows
{
    None,
    Direct,
    All,
};

SystemFileRows parseSystemFileRows(const std::string& value);

struct FileRegistryOptions
{
    std::vector<std::string> includeGlobs;
    std::vector<std::string> excludeGlobs;
    SystemFileRows systemFileRows = SystemFileRows::Direct;
};

class FileRegistry
{
public:
    struct Info
    {
        std::string path;
        bool system = false;
        bool indexed = false;
        bool directIncludeOfIndexed = false;
    };

    explicit FileRegistry(FileRegistryOptions options);

    // Looks up (creating on first sight) the Info for `file`. `tu` is
    // needed the first time to resolve clang_Location_isInSystemHeader.
    const Info& infoFor(CXTranslationUnit tu, CXFile file);

    // Records that `file` was seen as the direct #include target of an
    // indexed includer (called while walking clang_getInclusions()).
    void markDirectIncludeOfIndexed(CXTranslationUnit tu, CXFile file);

    // Whether `file` should get a FileFact row (and, transitively, be
    // eligible to act as a reference/edge endpoint): indexed files always
    // qualify; non-indexed files qualify per the systemFileRows policy
    // (none: never: direct: only if markDirectIncludeOfIndexed() was
    // called for it; all: always).
    bool shouldRegister(CXTranslationUnit tu, CXFile file);

private:
    struct FileKey
    {
        unsigned long long a, b, c;
        bool operator==(const FileKey& o) const { return a == o.a && b == o.b && c == o.c; }
    };
    struct FileKeyHash
    {
        std::size_t operator()(const FileKey& k) const
        {
            std::size_t h = std::hash<unsigned long long>{}(k.a);
            h ^= std::hash<unsigned long long>{}(k.b) + 0x9e3779b9 + (h << 6) + (h >> 2);
            h ^= std::hash<unsigned long long>{}(k.c) + 0x9e3779b9 + (h << 6) + (h >> 2);
            return h;
        }
    };

    static FileKey keyFor(CXFile file);
    Info& mutableInfoFor(CXTranslationUnit tu, CXFile file);

    FileRegistryOptions m_options;
    std::unordered_map<FileKey, Info, FileKeyHash> m_files;
};

}  // namespace kg::index
