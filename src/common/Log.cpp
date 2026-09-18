#include "kg/common/Log.h"

#include <cstdio>

namespace kg::common
{

void logInfo(const std::string& message)
{
    std::fprintf(stderr, "[kg] %s\n", message.c_str());
}

}  // namespace kg::common
