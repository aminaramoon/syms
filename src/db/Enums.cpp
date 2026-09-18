#include "kg/db/Enums.h"

#include <algorithm>
#include <array>
#include <cctype>

namespace kg::db
{

namespace
{

struct Entry
{
    int value;
    const char* name;
};

// clang-format off
constexpr std::array<Entry, 21> kNodeKindTable{{
    {NodeKind::UNKNOWN, "UNKNOWN"},
    {NodeKind::TYPE, "TYPE"},
    {NodeKind::BUILTIN_TYPE, "BUILTIN_TYPE"},
    {NodeKind::MODULE, "MODULE"},
    {NodeKind::NAMESPACE, "NAMESPACE"},
    {NodeKind::PACKAGE, "PACKAGE"},
    {NodeKind::STRUCT, "STRUCT"},
    {NodeKind::CLASS, "CLASS"},
    {NodeKind::INTERFACE, "INTERFACE"},
    {NodeKind::ANNOTATION, "ANNOTATION"},
    {NodeKind::GLOBAL_VARIABLE, "GLOBAL_VARIABLE"},
    {NodeKind::FIELD, "FIELD"},
    {NodeKind::FUNCTION, "FUNCTION"},
    {NodeKind::METHOD, "METHOD"},
    {NodeKind::ENUM, "ENUM"},
    {NodeKind::ENUM_CONSTANT, "ENUM_CONSTANT"},
    {NodeKind::TYPEDEF, "TYPEDEF"},
    {NodeKind::TYPE_PARAMETER, "TYPE_PARAMETER"},
    {NodeKind::FILE, "FILE"},
    {NodeKind::MACRO, "MACRO"},
    {NodeKind::UNION, "UNION"},
}};

constexpr std::array<Entry, 13> kEdgeKindTable{{
    {EdgeKind::UNKNOWN, "UNKNOWN"},
    {EdgeKind::MEMBER, "MEMBER"},
    {EdgeKind::TYPE_USAGE, "TYPE_USAGE"},
    {EdgeKind::USAGE, "USAGE"},
    {EdgeKind::CALL, "CALL"},
    {EdgeKind::INHERITANCE, "INHERITANCE"},
    {EdgeKind::OVERRIDE, "OVERRIDE"},
    {EdgeKind::TYPE_ARGUMENT, "TYPE_ARGUMENT"},
    {EdgeKind::TEMPLATE_SPECIALIZATION, "TEMPLATE_SPECIALIZATION"},
    {EdgeKind::INCLUDE, "INCLUDE"},
    {EdgeKind::IMPORT, "IMPORT"},
    {EdgeKind::MACRO_USAGE, "MACRO_USAGE"},
    {EdgeKind::ANNOTATION_USAGE, "ANNOTATION_USAGE"},
}};

constexpr std::array<Entry, 2> kDefinitionKindTable{{
    {DefinitionKind::IMPLICIT, "IMPLICIT"},
    {DefinitionKind::EXPLICIT, "EXPLICIT"},
}};

constexpr std::array<Entry, 10> kLocationKindTable{{
    {LocationKind::TOKEN, "TOKEN"},
    {LocationKind::SCOPE, "SCOPE"},
    {LocationKind::QUALIFIER, "QUALIFIER"},
    {LocationKind::LOCAL_SYMBOL, "LOCAL_SYMBOL"},
    {LocationKind::SIGNATURE, "SIGNATURE"},
    {LocationKind::ATOMIC_RANGE, "ATOMIC_RANGE"},
    {LocationKind::INDEXER_ERROR, "INDEXER_ERROR"},
    {LocationKind::FULLTEXT_SEARCH, "FULLTEXT_SEARCH"},
    {LocationKind::SCREEN_SEARCH, "SCREEN_SEARCH"},
    {LocationKind::UNSOLVED, "UNSOLVED"},
}};
// clang-format on

std::string toUpper(const std::string& s)
{
    std::string out = s;
    std::transform(out.begin(), out.end(), out.begin(), [](unsigned char c) { return static_cast<char>(std::toupper(c)); });
    return out;
}

template <std::size_t N>
std::string toStringImpl(const std::array<Entry, N>& table, int value)
{
    for (const Entry& e : table)
    {
        if (e.value == value)
        {
            return e.name;
        }
    }
    return "UNKNOWN(" + std::to_string(value) + ")";
}

template <std::size_t N>
std::optional<int> fromStringImpl(const std::array<Entry, N>& table, const std::string& name)
{
    const std::string upper = toUpper(name);
    for (const Entry& e : table)
    {
        if (upper == e.name)
        {
            return e.value;
        }
    }
    return std::nullopt;
}

}  // namespace

bool isNamespaceLikeNodeKind(int kind)
{
    return kind == NodeKind::NAMESPACE || kind == NodeKind::MODULE || kind == NodeKind::PACKAGE;
}

std::string nodeKindToString(int kind)
{
    return toStringImpl(kNodeKindTable, kind);
}

std::string edgeKindToString(int kind)
{
    return toStringImpl(kEdgeKindTable, kind);
}

std::string definitionKindToString(int kind)
{
    return toStringImpl(kDefinitionKindTable, kind);
}

std::string locationKindToString(int kind)
{
    return toStringImpl(kLocationKindTable, kind);
}

std::optional<int> nodeKindFromString(const std::string& name)
{
    return fromStringImpl(kNodeKindTable, name);
}

std::optional<int> edgeKindFromString(const std::string& name)
{
    return fromStringImpl(kEdgeKindTable, name);
}

std::optional<int> definitionKindFromString(const std::string& name)
{
    return fromStringImpl(kDefinitionKindTable, name);
}

std::optional<int> locationKindFromString(const std::string& name)
{
    return fromStringImpl(kLocationKindTable, name);
}

}  // namespace kg::db
