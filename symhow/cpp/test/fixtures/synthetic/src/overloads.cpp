#include "macros.hpp"
#include "shapes.hpp"

#include <string>

// Free-function overload set, plus a class with a const/non-const overload
// of the same member function name.

int f(int x)
{
    return x + 1;
}

double f(double x)
{
    return x + 1.0;
}

int f(const std::string& s)
{
    return static_cast<int>(s.size());
}

class Accessor
{
public:
    int& value() { return m_value; }
    const int& value() const { return m_value; }

private:
    int m_value = 0;
};

int callOverloads()
{
    Accessor a;
    const Accessor& constA = a;

    fixture::Colored* c = nullptr;
    (void)c;

    int fromInt = f(1);
    double fromDouble = f(2.5);
    int fromString = f(std::string("hi"));
    int squared = FIXTURE_SQUARE(fromInt);

    return fromInt + static_cast<int>(fromDouble) + fromString + squared + a.value() + constA.value();
}
