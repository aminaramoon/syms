#include "kg/common/Paths.h"

#include <filesystem>
#include <system_error>

namespace kg::common::paths
{

std::string normalize(const std::string& path)
{
    std::filesystem::path p(path);
    std::error_code ec;
    if (!p.is_absolute())
    {
        const std::filesystem::path abs = std::filesystem::absolute(p, ec);
        if (!ec)
        {
            p = abs;
        }
    }

    std::error_code existsEc;
    if (std::filesystem::exists(p, existsEc) && !existsEc)
    {
        std::error_code canonEc;
        const std::filesystem::path canon = std::filesystem::weakly_canonical(p, canonEc);
        if (!canonEc)
        {
            return canon.string();
        }
    }
    return p.lexically_normal().string();
}

const std::string& NormalizationCache::operator()(const std::string& path)
{
    const auto it = m_paths.find(path);
    if (it != m_paths.end())
    {
        return it->second;
    }
    return m_paths.emplace(path, normalize(path)).first->second;
}

}  // namespace kg::common::paths
