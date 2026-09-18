#include "kg/index/FileRegistry.h"

#include <stdexcept>

#include "kg/common/Glob.h"
#include "kg/index/ClangUtil.h"

namespace kg::index
{

namespace
{

bool matchesAny(const std::vector<std::string>& globs, const std::string& path)
{
    for (const auto& g : globs)
    {
        if (kg::common::glob::match(g, path))
        {
            return true;
        }
    }
    return false;
}

}  // namespace

SystemFileRows parseSystemFileRows(const std::string& value)
{
    if (value == "none")
    {
        return SystemFileRows::None;
    }
    if (value == "direct")
    {
        return SystemFileRows::Direct;
    }
    if (value == "all")
    {
        return SystemFileRows::All;
    }
    throw std::invalid_argument("invalid --system-file-rows value '" + value + "' (expected none, direct, or all)");
}

FileRegistry::FileRegistry(FileRegistryOptions options) : m_options(std::move(options)) {}

FileRegistry::FileKey FileRegistry::keyFor(CXFile file)
{
    CXFileUniqueID id{};
    if (clang_getFileUniqueID(file, &id) != 0)
    {
        // Fall back to the raw pointer if libclang can't give us a unique
        // id (shouldn't happen in practice for a valid CXFile).
        return FileKey{reinterpret_cast<unsigned long long>(file), 0, 0};
    }
    return FileKey{id.data[0], id.data[1], id.data[2]};
}

FileRegistry::Info& FileRegistry::mutableInfoFor(CXTranslationUnit tu, CXFile file)
{
    const FileKey key = keyFor(file);
    auto it = m_files.find(key);
    if (it != m_files.end())
    {
        return it->second;
    }

    Info info;
    info.path = clangutil::fileName(file);
    info.system = clang_Location_isInSystemHeader(clang_getLocationForOffset(tu, file, 0)) != 0;
    info.indexed = !info.system && (m_options.includeGlobs.empty() || matchesAny(m_options.includeGlobs, info.path)) &&
                   !matchesAny(m_options.excludeGlobs, info.path);

    auto [inserted, ok] = m_files.emplace(key, std::move(info));
    (void)ok;
    return inserted->second;
}

const FileRegistry::Info& FileRegistry::infoFor(CXTranslationUnit tu, CXFile file)
{
    return mutableInfoFor(tu, file);
}

void FileRegistry::markDirectIncludeOfIndexed(CXTranslationUnit tu, CXFile file)
{
    mutableInfoFor(tu, file).directIncludeOfIndexed = true;
}

bool FileRegistry::shouldRegister(CXTranslationUnit tu, CXFile file)
{
    const Info& info = infoFor(tu, file);
    if (info.indexed)
    {
        return true;
    }
    switch (m_options.systemFileRows)
    {
        case SystemFileRows::None:
            return false;
        case SystemFileRows::Direct:
            return info.directIncludeOfIndexed;
        case SystemFileRows::All:
            return true;
    }
    return false;
}

}  // namespace kg::index
