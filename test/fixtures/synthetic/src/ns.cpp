#include "macros.hpp"
#include "shapes.hpp"

// Nested namespaces, an anonymous namespace, a typedef + a using alias, an
// enum class with constants, a global variable, and a struct with fields.

namespace a
{
namespace b
{
namespace c
{

int add(int x, int y)
{
    return x + y;
}

}  // namespace c
}  // namespace b
}  // namespace a

namespace
{

int anonymousHelper(int x)
{
    return x * 2;
}

}  // namespace

typedef int LegacyInt;
using ModernInt = int;

enum class Color
{
    Red,
    Green,
    Blue,
};

struct Point
{
    ModernInt x;
    ModernInt y;
};

int gGlobalCounter = FIXTURE_VERSION;

int useEverything()
{
    Point p{1, 2};
    LegacyInt li = p.x;
    Color c = Color::Green;
    int squared = FIXTURE_SQUARE(li);
    fixture::Square sq(3.0);
    return a::b::c::add(li, squared) + anonymousHelper(static_cast<int>(c)) + gGlobalCounter +
           static_cast<int>(sq.area());
}
