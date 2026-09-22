#pragma once

// A 3-level multiple-inheritance hierarchy, included by two different TUs
// (ns.cpp and overloads.cpp) to exercise header-dedup: the exact same
// USR-derived SymbolKeys must resolve to the exact same nodes no matter
// which TU is indexed first.

namespace fixture
{

class Shape
{
public:
    virtual ~Shape() = default;
    virtual double area() const = 0;
};

class Polygon : public Shape
{
public:
    double area() const override { return 0.0; }
};

class Colored : public Shape
{
public:
    double area() const override { return 0.0; }
    virtual const char* color() const { return "none"; }
};

class Square : public Polygon, public Colored
{
public:
    explicit Square(double side) : m_side(side) {}

    double area() const override { return m_side * m_side; }
    const char* color() const override { return "unknown"; }

private:
    double m_side;
};

// Declared here, defined out-of-line in ns.cpp -- exercises kg_symbol_def
// getting a row for BOTH the header (declaration) and the .cpp (definition)
// for the same symbol, and definition_kind landing on Explicit (from the
// out-of-line definition) rather than staying at None.
double squarePerimeter(const Square& s);

}  // namespace fixture
