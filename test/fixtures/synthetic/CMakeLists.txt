# Synthetic fixture project for kg's Phase 3 (and later) verification.
#
# This project is never built by the main kg CMake configure/build -- it is
# only ever *configured* (by the `fixture_cdb_generate` CTest test, see
# ../CMakeLists.txt) so that CMAKE_EXPORT_COMPILE_COMMANDS=ON produces a
# compile_commands.json for `kg index` to consume. `cmake --build` on it is
# cheap and was used during development to shake out syntax errors, but is
# not part of the verification flow itself.
cmake_minimum_required(VERSION 3.20)
project(kg_fixture LANGUAGES C CXX)

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

# cfile.c: a plain C translation unit (separate target so its
# compile_commands.json entry gets a C, not C++, compiler driver -- e.g.
# `cc`, not `c++`/`g++`), exercising the ArgSanitizer language-aware -std
# fallback (a .c file must get a C -std, never a C++ one).
add_library(fixture_c STATIC src/cfile.c)

# fixture_lib_dup: compiles templates.cpp a SECOND time, with a different
# -D, so the fixture's compile_commands.json has 8 entries for only 7
# distinct `file` paths -- a real CDB shape (the same source compiled for
# several targets/configs with different flags) kg_tu's duplicate-CDB-entry
# handling needs to be exercised against (see kg::index::pathArgsHashes()
# and docs/KNOWN_LIMITATIONS.md). templates.cpp is deliberately chosen over
# chain.cpp (the other zero-#include TU) because it's ALREADY outside the
# first-3-CDB-entries window test_resume_fixture.cpp's --stop-after=3
# scenario relies on (fixture_lib lists ns.cpp, overloads.cpp, chain.cpp,
# templates.cpp in that order) -- duplicating chain.cpp instead would split
# one path's two entries across that test's interrupt boundary, which is a
# real (documented, not fixed here) limitation of the "write kg_tu on
# whichever entry is consumed first" design, not something this fixture is
# meant to exercise. KG_DUP=1 is otherwise unused by templates.cpp -- this
# entry's flags differ from the original's, but its AST content is
# identical, which is what real duplicated-but-differently-flagged TUs
# usually look like too (same code, different -D/-I picked by the build
# system per target).
add_library(fixture_lib_dup STATIC src/templates.cpp)
target_compile_definitions(fixture_lib_dup PRIVATE KG_DUP=1)
