# symhow first-party target graph.
#
# Phase 1: all of these targets exist and are wired together correctly, but
# most have only stub/placeholder sources. Later phases add real .cpp files
# to existing targets; the target graph itself (this file) should not need
# to be restructured.

set(SYMHOW_INCLUDE_DIR "${CMAKE_SOURCE_DIR}/include")

# --- symhow_common ---------------------------------------------------------
add_library(symhow_common STATIC
    common/Log.cpp
    common/Shell.cpp
    common/Glob.cpp
    common/Process.cpp
    common/Paths.cpp
    common/ContentHash.cpp
)
target_include_directories(symhow_common PUBLIC ${SYMHOW_INCLUDE_DIR})
target_link_libraries(symhow_common PRIVATE symhow_options)

# --- symhow_model ------------------------------------------------------------
add_library(symhow_model STATIC
    model/Model.cpp
    model/Enums.cpp
    model/BatchSink.cpp
)
target_link_libraries(symhow_model PUBLIC symhow_common)
target_link_libraries(symhow_model PRIVATE symhow_options)

# --- symhow_db ---------------------------------------------------------------
add_library(symhow_db STATIC
    db/Db.cpp
    db/Database.cpp
    db/Enums.cpp
    db/NameDecode.cpp
    db/WritableDb.cpp
    db/Meta.cpp
)
target_link_libraries(symhow_db PUBLIC SQLite::SQLite3 symhow_common)
target_link_libraries(symhow_db PRIVATE symhow_options)

# --- symhow_index --------------------------------------------------------------
add_library(symhow_index STATIC
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
target_link_libraries(symhow_index PUBLIC
    symhow_model
    symhow_db
    symhow_common
    kg_sourcetraildb
    LibClang::LibClang
    nlohmann_json::nlohmann_json
    Threads::Threads
)
target_link_libraries(symhow_index PRIVATE symhow_options)

# Bake the resource dir chosen by cmake/FindLibClang.cmake in as a compile
# definition. symhow_index has no real libclang usage yet (Phase 1), but later
# phases can just reference this macro rather than re-deriving the path.
target_compile_definitions(symhow_index PRIVATE
    SYMHOW_DEFAULT_RESOURCE_DIR="${LibClang_RESOURCE_DIR}"
)

# --- symhow_query --------------------------------------------------------------
add_library(symhow_query STATIC
    query/Query.cpp
    query/Bfs.cpp
    query/SymbolResolver.cpp
    query/FuzzyMatcher.cpp
    query/QueryEngine.cpp
)
target_link_libraries(symhow_query PUBLIC symhow_db symhow_common)
target_link_libraries(symhow_query PRIVATE symhow_options)

# --- symhow_update ---------------------------------------------------------------
# Phase 2: the pure/testable pieces of `symhow update` (git diff/status parsing,
# changed-file classification against the DB/CDB, and the logical DB
# snapshot used for equivalence testing) plus the hidden `symhow dbdiff` CLI
# command built on top of Snapshot. Does NOT include the invalidation SQL or
# the `update` command itself -- that's Phase 3, added to this same target.
add_library(symhow_update STATIC
    update/GitDiff.cpp
    update/Git.cpp
    update/FileMatcher.cpp
    update/Snapshot.cpp
    update/Invalidator.cpp
    update/UpdateDriver.cpp
)
target_link_libraries(symhow_update PUBLIC symhow_index symhow_query symhow_db symhow_common nlohmann_json::nlohmann_json)
target_link_libraries(symhow_update PRIVATE symhow_options)

# --- symhow_notes ----------------------------------------------------------------
# Phase 1 of docs/NOTES_DESIGN.md: the markdown model/parser/renderer for
# `symhow notes` files and `symhow notes lint`. No DB dependency in this phase (that
# arrives in Phase 2's NotesStore, added to this same target).
add_library(symhow_notes STATIC
    notes/Markdown.cpp
    notes/BodyHash.cpp
    notes/Sync.cpp
    notes/NotesStore.cpp
    notes/Scaffold.cpp
)
target_link_libraries(symhow_notes PUBLIC symhow_common symhow_db symhow_query nlohmann_json::nlohmann_json)
target_link_libraries(symhow_notes PRIVATE symhow_options)

# Phase 3: `symhow update`'s post-index notes sync (UpdateDriver.cpp) needs
# symhow::notes::syncNotes() -- linked here, after symhow_notes is declared, rather
# than alongside symhow_update's other PUBLIC deps above (symhow_notes didn't exist
# as a target yet at that point in this file).
target_link_libraries(symhow_update PUBLIC symhow_notes)

# --- symhow_out ------------------------------------------------------------------
add_library(symhow_out STATIC
    out/Out.cpp
    out/TextWriter.cpp
    out/JsonWriter.cpp
)
target_link_libraries(symhow_out PUBLIC symhow_query symhow_db nlohmann_json::nlohmann_json)
target_link_libraries(symhow_out PRIVATE symhow_options)

# --- symhow (CLI executable) ----------------------------------------------------
add_executable(symhow
    cli/Cli.cpp
    main.cpp
)
target_link_libraries(symhow PRIVATE
    symhow_out
    symhow_update
    symhow_index
    symhow_query
    symhow_notes
    symhow_db
    symhow_model
    symhow_common
    symhow_options
)
target_compile_definitions(symhow PRIVATE
    SYMHOW_VERSION_STRING="${PROJECT_VERSION}"
    SYMHOW_LIBCLANG_VERSION="${LibClang_VERSION}"
    SYMHOW_SQLITE_VERSION="${SQLite3_VERSION}"
    SYMHOW_SRCTRLDB_VERSION_STRING="${KG_SOURCETRAILDB_VERSION_STRING}"
)

# Place the CLI binary directly at <build>/symhow (not <build>/src/symhow) so it's
# where the top-level build/test instructions expect it.
set_target_properties(symhow PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}"
)
