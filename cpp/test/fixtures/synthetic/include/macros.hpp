#pragma once

// An object-like and a function-like macro, both used in two different TUs
// (ns.cpp and overloads.cpp) -- exercises MACRO symbol dedup and
// MACRO_USAGE reference counting across TUs.

#define FIXTURE_VERSION 3

#define FIXTURE_SQUARE(x) ((x) * (x))
