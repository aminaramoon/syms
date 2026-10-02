# kg first-party target graph.
#
# Phase 1: all of these targets exist and are wired together correctly, but
# most have only stub/placeholder sources. Later phases add real .cpp files
# to existing targets; the target graph itself (this file) should not need
# to be restructured.

set(KG_INCLUDE_DIR "${CMAKE_SOURCE_DIR}/include")

# --- kg_common ---------------------------------------------------------
add_library(kg_common STATIC
    common/Log.cpp
    common/Shell.cpp
    common/Glob.cpp
    common/Process.cpp
    common/Paths.cpp
)
target_include_directories(kg_common PUBLIC ${KG_INCLUDE_DIR})
target_link_libraries(kg_common PRIVATE kg_options)

# --- kg_model ------------------------------------------------------------
add_library(kg_model STATIC
    model/Model.cpp
    model/Enums.cpp
    model/BatchSink.cpp
)
target_link_libraries(kg_model PUBLIC kg_common)
target_link_libraries(kg_model PRIVATE kg_options)

# --- kg_db ---------------------------------------------------------------
add_library(kg_db STATIC
    db/Db.cpp
    db/Database.cpp
    db/Enums.cpp
    db/NameDecode.cpp
    db/WritableDb.cpp
    db/Meta.cpp
)
target_link_libraries(kg_db PUBLIC SQLite::SQLite3 kg_common)
target_link_libraries(kg_db PRIVATE kg_options)

# --- kg_index --------------------------------------------------------------
add_library(kg_index STATIC
    index/Index.cpp
    index/CompileDb.cpp
    index/ArgSanitizer.cpp
    index/CudaSupport.cpp
    index/NameBuilder.cpp
    index/FileRegistry.cpp
    index/TuParser.cpp
    index/AstWalker.cpp
    index/SrctrldbBackend.cpp
    index/IndexDriver.cpp
    index/Provenance.cpp
    index/Progress.cpp
)
find_package(Threads REQUIRED)
target_link_libraries(kg_index PUBLIC
    kg_model
    kg_db
    kg_common
    kg_sourcetraildb
    LibClang::LibClang
    nlohmann_json::nlohmann_json
    Threads::Threads
)
target_link_libraries(kg_index PRIVATE kg_options)

# Bake the resource dir chosen by cmake/FindLibClang.cmake in as a compile
# definition. kg_index has no real libclang usage yet (Phase 1), but later
# phases can just reference this macro rather than re-deriving the path.
target_compile_definitions(kg_index PRIVATE
    KG_DEFAULT_RESOURCE_DIR="${LibClang_RESOURCE_DIR}"
)

# --- kg_query --------------------------------------------------------------
add_library(kg_query STATIC
    query/Query.cpp
    query/Bfs.cpp
    query/SymbolResolver.cpp
    query/FuzzyMatcher.cpp
    query/QueryEngine.cpp
)
target_link_libraries(kg_query PUBLIC kg_db kg_common)
target_link_libraries(kg_query PRIVATE kg_options)

# --- kg_update ---------------------------------------------------------------
# Phase 2: the pure/testable pieces of `kg update` (git diff/status parsing,
# changed-file classification against the DB/CDB, and the logical DB
# snapshot used for equivalence testing) plus the hidden `kg dbdiff` CLI
# command built on top of Snapshot. Does NOT include the invalidation SQL or
# the `update` command itself -- that's Phase 3, added to this same target.
add_library(kg_update STATIC
    update/GitDiff.cpp
    update/Git.cpp
    update/FileMatcher.cpp
    update/Snapshot.cpp
    update/Invalidator.cpp
    update/UpdateDriver.cpp
)
target_link_libraries(kg_update PUBLIC kg_index kg_query kg_db kg_common nlohmann_json::nlohmann_json)
target_link_libraries(kg_update PRIVATE kg_options)

# --- kg_notes ----------------------------------------------------------------
# Phase 1 of docs/NOTES_DESIGN.md: the markdown model/parser/renderer for
# `kg notes` files and `kg notes lint`. No DB dependency in this phase (that
# arrives in Phase 2's NotesStore, added to this same target).
add_library(kg_notes STATIC
    notes/Markdown.cpp
    notes/BodyHash.cpp
    notes/Sync.cpp
    notes/NotesStore.cpp
    notes/Scaffold.cpp
)
target_link_libraries(kg_notes PUBLIC kg_common kg_db kg_query nlohmann_json::nlohmann_json)
target_link_libraries(kg_notes PRIVATE kg_options)

# Phase 3: `kg update`'s post-index notes sync (UpdateDriver.cpp) needs
# kg::notes::syncNotes() -- linked here, after kg_notes is declared, rather
# than alongside kg_update's other PUBLIC deps above (kg_notes didn't exist
# as a target yet at that point in this file).
target_link_libraries(kg_update PUBLIC kg_notes)

# --- kg_out ------------------------------------------------------------------
add_library(kg_out STATIC
    out/Out.cpp
    out/TextWriter.cpp
    out/JsonWriter.cpp
)
target_link_libraries(kg_out PUBLIC kg_query kg_db nlohmann_json::nlohmann_json)
target_link_libraries(kg_out PRIVATE kg_options)

# --- kg (CLI executable) ----------------------------------------------------
add_executable(kg
    cli/Cli.cpp
    main.cpp
)
target_link_libraries(kg PRIVATE
    kg_out
    kg_update
    kg_index
    kg_query
    kg_notes
    kg_db
    kg_model
    kg_common
    kg_options
)
target_compile_definitions(kg PRIVATE
    KG_VERSION_STRING="${PROJECT_VERSION}"
    KG_LIBCLANG_VERSION="${LibClang_VERSION}"
    KG_SQLITE_VERSION="${SQLite3_VERSION}"
    KG_SRCTRLDB_VERSION_STRING="${KG_SOURCETRAILDB_VERSION_STRING}"
)

# Place the CLI binary directly at <build>/kg (not <build>/src/kg) so it's
# where the top-level build/test instructions expect it.
set_target_properties(kg PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}"
)
