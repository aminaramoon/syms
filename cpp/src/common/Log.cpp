#include "symhow/common/Log.h"

#include <cstdio>

namespace symhow::common
{

void logInfo(const std::string& message)
{
    std::fprintf(stderr, "[symhow] %s\n", message.c_str());
}

}  // namespace symhow::common
