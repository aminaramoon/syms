# Synthetic fixture project for kg's Phase 3 (and later) verification.
#
# This project is never built by the main kg CMake configure/build -- it is
# only ever *configured* (by the `fixture_cdb_generate` CTest test, see
# ../CMakeLists.txt) so that CMAKE_EXPORT_COMPILE_COMMANDS=ON produces a
# compile_commands.json for `kg index` to consume. `cmake --build` on it is
# cheap and was used during development to shake out syntax errors, but is
# not part of the verification flow itself.
cmake_minimum_required(VERSION 3.20)
project(kg_fixture CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

include_directories(${CMAKE_CURRENT_SOURCE_DIR}/include)

# ns.cpp / overloads.cpp / chain.cpp / templates.cpp don't define main(), so
# they're gathered into one library target (no link step needed to produce
# useful compile_commands.json entries).
add_library(fixture_lib STATIC
    src/ns.cpp
    src/overloads.cpp
    src/chain.cpp
    src/templates.cpp
)

# tu_a.cpp / tu_b.cpp each define their own int main() (the deliberate
# USR-collision test case) -- kept as separate executables so both can
# build/link independently despite each having a main().
add_executable(tu_a src/tu_a.cpp)
add_executable(tu_b src/tu_b.cpp)
