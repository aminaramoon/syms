#include "kg/model/Enums.h"

namespace kg::model
{

int kindRank(SymbolKind kind)
{
    switch (kind)
    {
        case SymbolKind::Unknown:
            return 0;
        case SymbolKind::Type:
            return 1;
        default:
            return 2;
    }
}

}  // namespace kg::model
