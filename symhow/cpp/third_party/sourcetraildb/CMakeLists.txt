# Vendored copy of SourcetrailDB's core writer library (Apache-2.0).
# See PROVENANCE.md for exact upstream commit/version and any patches.
#
# Built as a static library at C++17 (not the project-wide C++20): these
# sources are from 2018 and predate some C++20 breaking changes (e.g.
# stricter implicit-capture-of-this-in-lambda rules, comparison operator
# changes). Only plain std::string / std::vector / POD structs cross the
# boundary into kg_index, so mixing standards across this static-lib
# boundary is ABI-safe on the platforms we target (Itanium C++ ABI,
# libstdc++).

set(KG_SOURCETRAILDB_VERSION_STRING "v4.db25.p1" CACHE INTERNAL "SourcetrailDB schema version implemented by third_party/sourcetraildb")
set(KG_SOURCETRAILDB_VERSION_STRING "v4.db25.p1" PARENT_SCOPE)

add_library(kg_sourcetraildb STATIC
    src/DatabaseStorage.cpp
    src/DefinitionKind.cpp
    src/EdgeKind.cpp
    src/ElementComponentKind.cpp
    src/LocationKind.cpp
    src/NameHierarchy.cpp
    src/NodeKind.cpp
    src/ReferenceKind.cpp
    src/SourcetrailDBWriter.cpp
    src/SymbolKind.cpp
    src/utility.cpp
    cpp_sqlite/CppSQLite3.cpp
)

set_target_properties(kg_sourcetraildb PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
    CXX_EXTENSIONS OFF
)

# Vendored code isn't held to kg's own warning bar: it's unmodified
# upstream source, and modern GCC is considerably noisier than the 2018-era
# compilers this code was written against.
target_compile_options(kg_sourcetraildb PRIVATE -w)

target_include_directories(kg_sourcetraildb
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/include
        ${CMAKE_CURRENT_SOURCE_DIR}/cpp_sqlite   # DatabaseStorage.h (public) does #include "CppSQLite3.h"
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}       # for #include "version.h" (implementation detail only)
)

# NameHierarchy.cpp does `#include "json.hpp"` (no "nlohmann/" prefix,
# quote-form include). flox's nlohmann_json package ships headers under
# <prefix>/include/nlohmann/json.hpp, so point the include path directly at
# the "nlohmann" subdirectory. SYSTEM so warnings in the vendored/third-party
# header are suppressed. PRIVATE: this include is only needed while building
# the vendored .cpp files themselves, not by consumers of the public headers.
target_include_directories(kg_sourcetraildb SYSTEM PRIVATE
    "${nlohmann_json_INCLUDE_DIR}/nlohmann"
)

# CppSQLite3.h (public, see above) does `#include "sqlite3.h"`; use flox's
# sqlite3 dev headers instead of vendoring sqlite3.c/sqlite3.h ourselves.
target_include_directories(kg_sourcetraildb SYSTEM PUBLIC
    ${SQLite3_INCLUDE_DIRS}
)

target_link_libraries(kg_sourcetraildb PUBLIC SQLite::SQLite3)
