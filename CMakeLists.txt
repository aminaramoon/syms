cmake_minimum_required(VERSION 3.20)
project(kg VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
list(APPEND CMAKE_MODULE_PATH ${CMAKE_SOURCE_DIR}/cmake)

option(KG_BUILD_TESTS "Build unit tests" ON)
option(KG_BUILD_INTEGRATION_TESTS "Register the cucs integration test" ON)
set(KG_CUCS_DIR "/home/aaramoon/workspace/oss/cucs" CACHE PATH "cucs corpus root")

include(KgWarnings)                        # defines INTERFACE target kg_options
find_package(LibClang REQUIRED)            # cmake/FindLibClang.cmake -> LibClang::LibClang
find_package(SQLite3 REQUIRED)             # CMake builtin FindSQLite3 -> SQLite::SQLite3
find_package(nlohmann_json REQUIRED CONFIG)  # flox-provided, exports nlohmann_json::nlohmann_json

# nlohmann_json's own Config file does not export a conventional
# `nlohmann_json_INCLUDE_DIR` variable (it's a CONFIG-mode package, not a
# classic Find-module), so derive one from the imported target for anything
# downstream (e.g. third_party/sourcetraildb) that needs raw include-dir
# access rather than target_link_libraries().
get_target_property(_kg_nlohmann_json_include_dirs
    nlohmann_json::nlohmann_json INTERFACE_INCLUDE_DIRECTORIES)
list(GET _kg_nlohmann_json_include_dirs 0 nlohmann_json_INCLUDE_DIR)
unset(_kg_nlohmann_json_include_dirs)

if(KG_BUILD_INTEGRATION_TESTS AND NOT EXISTS "${KG_CUCS_DIR}")
    message(WARNING "KG_CUCS_DIR (${KG_CUCS_DIR}) does not exist; integration tests that depend on it will be skipped/stubbed until Phase 6.")
endif()

enable_testing()

add_subdirectory(third_party/sourcetraildb)
add_subdirectory(src)

if(KG_BUILD_TESTS)
    add_subdirectory(test)
endif()

# --- Configuration summary --------------------------------------------------
# This is intentionally verbose: it's the primary tool later phases and
# end users have for debugging environment/toolchain mismatches (wrong
# libclang picked up, resource dir missing, wrong sqlite, etc).
message(STATUS "")
message(STATUS "kg configuration summary")
message(STATUS "  kg version              : ${PROJECT_VERSION}")
message(STATUS "  C++ standard            : ${CMAKE_CXX_STANDARD}")
message(STATUS "  Build type              : ${CMAKE_BUILD_TYPE}")
message(STATUS "  Generator               : ${CMAKE_GENERATOR}")
message(STATUS "  libclang version        : ${LibClang_VERSION}")
message(STATUS "  libclang library        : ${LibClang_LIBRARY}")
message(STATUS "  libclang include dir    : ${LibClang_INCLUDE_DIR}")
message(STATUS "  libclang resource dir   : ${LibClang_RESOURCE_DIR}")
message(STATUS "  sqlite3 version         : ${SQLite3_VERSION}")
message(STATUS "  sqlite3 include dirs    : ${SQLite3_INCLUDE_DIRS}")
message(STATUS "  sqlite3 libraries       : ${SQLite3_LIBRARIES}")
message(STATUS "  nlohmann_json version   : ${nlohmann_json_VERSION}")
message(STATUS "  nlohmann_json include   : ${nlohmann_json_INCLUDE_DIR}")
message(STATUS "  sourcetraildb schema    : ${KG_SOURCETRAILDB_VERSION_STRING}")
message(STATUS "  KG_CUCS_DIR             : ${KG_CUCS_DIR}")
message(STATUS "  KG_BUILD_TESTS          : ${KG_BUILD_TESTS}")
message(STATUS "  KG_BUILD_INTEGRATION_TESTS : ${KG_BUILD_INTEGRATION_TESTS}")
message(STATUS "  KG_STRICT               : ${KG_STRICT}")
message(STATUS "")
