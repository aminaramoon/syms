# Provenance: vendored SourcetrailDB core

This directory contains a verbatim vendored copy of the "core" writer
library from the [SourcetrailDB](https://github.com/CoatiSoftware/SourcetrailDB)
project, used by `kg` to write Sourcetrail-schema-compatible `.srctrldb`
SQLite databases.

- Upstream repository (local checkout used for vendoring): `/home/aaramoon/workspace/mine/knowledge_graph/SourcetrailDB`
- Upstream license: Apache License 2.0 (see `LICENSE.txt` in this directory, copied verbatim)
- Upstream schema version: `v4.db25.p1` (from upstream `version.txt`)
  - `INTERFACE_VERSION` = 4
  - `DATABASE_VERSION`  = 25 (this is the Sourcetrail-schema/database version kg targets)
  - `PATCH_VERSION`     = 1

## What was copied

Copied verbatim, byte-for-byte, with no modifications:

- `core/include/*.h` (22 headers) -> `include/`
- `core/src/*.cpp` (11 sources) -> `src/`
- `external/cpp_sqlite/include/CppSQLite3.h` and
  `external/cpp_sqlite/src/CppSQLite3.cpp` -> `cpp_sqlite/`
  (NOTE: upstream's own `sqlite3.c`/`sqlite3.h` bundled next to CppSQLite3
  were intentionally **not** copied; kg links against flox's SQLite3
  package instead — see the root CMakeLists.txt `find_package(SQLite3)`.)
- `LICENSE.txt`, `AUTHORS.txt` -> copied verbatim to this directory.

## What was hand-written (not copied)

- `version.h` — upstream generates `core/version.h.in` via CMake's
  `configure_file()` at upstream-build time, substituting
  `INTERFACE_VERSION` / `DATABASE_VERSION` / `PATCH_VERSION` /
  `VERSION_STRING` from `version.txt` using the regexes in upstream's
  top-level `CMakeLists.txt`:
  ```
  v([0-9]+)\..*             -> INTERFACE_VERSION = 4
  v[0-9]+\.db([0-9]+)\..*   -> DATABASE_VERSION  = 25
  v[0-9]+\.db[0-9]+\.p([0-9]+).* -> PATCH_VERSION = 1
  ```
  kg does not run that configure step (no build-time dependency on a
  `version.txt`/`version.h.in` templating step), so `version.h` here is a
  plain, non-templated header hard-coding the exact same macro names to
  the exact same values. Consumed by `src/DatabaseStorage.cpp`
  (`DATABASE_VERSION`, via `getSupportedDatabaseVersion()`, which is what
  ends up written to the `meta` table's `storage_version` row) and
  `src/SourcetrailDBWriter.cpp` (`VERSION_STRING`, via
  `getVersionString()`).
- `CMakeLists.txt` in this directory — kg's own build wiring for the
  vendored sources (target `kg_sourcetraildb`); no equivalent file is
  vendored from upstream's `core/CMakeLists.txt`, though the source-file
  list and general include-dir shape (vendored include/, cpp_sqlite,
  external json headers) mirror it.

## Patches

**No patches applied to any vendored `.h`/`.cpp` file.** All compilation
issues (the `#include "json.hpp"` and `#include "sqlite3.h"` quote-style
includes expecting sibling-directory headers that we don't vendor
ourselves) were resolved purely via CMake `target_include_directories()`
in this directory's `CMakeLists.txt` — pointing an include path directly
at flox's `nlohmann_json` package's `include/nlohmann` directory (so
`#include "json.hpp"` resolves to `.../nlohmann/json.hpp`), and at flox's
SQLite3 `SQLite3_INCLUDE_DIRS` (so `#include "sqlite3.h"` resolves). No
source file modification was necessary.

## C++ standard

Built at **C++17** (the rest of the kg tree is C++20). These sources date
from 2018 and predate several C++20 breaking-change areas; C++17 was
chosen defensively to reduce build risk, per the Phase 1 build spec. Only
plain `std::string` / `std::vector` / POD structs cross the static-library
boundary into `kg_index`, so mixing standards across that boundary is
ABI-safe (Itanium C++ ABI / libstdc++). Built and verified with GCC
15.3.0 (flox `gcc` package) at C++17 with no errors and no fallback to
C++14 needed.
