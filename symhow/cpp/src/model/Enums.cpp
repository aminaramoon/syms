#include "symhow/model/Enums.h"

namespace symhow::model
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

bool symbolKindBeats(SymbolKind candidate, SymbolKind current)
{
    const int candidateRank = kindRank(candidate);
    const int currentRank = kindRank(current);
    if (candidateRank != currentRank)
    {
        return candidateRank > currentRank;
    }
    return static_cast<int>(candidate) > static_cast<int>(current);
}

}  // namespace symhow::model
