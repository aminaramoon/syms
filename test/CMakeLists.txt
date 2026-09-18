find_package(Catch2 3 REQUIRED)
include(Catch)

# --- kg_unit_tests -----------------------------------------------------------
add_executable(kg_unit_tests
    unit/test_writer_smoke.cpp
    unit/test_shell_split.cpp
    unit/test_glob.cpp
    unit/test_arg_sanitizer.cpp
    unit/test_cuda_support.cpp
    unit/test_cdb_samples.cpp
    unit/test_name_decode.cpp
    unit/test_fuzzy.cpp
    unit/test_bfs.cpp
    unit/test_query_fixture.cpp
    unit/test_json_shape.cpp
    unit/test_cuda_fixture.cpp
)
target_link_libraries(kg_unit_tests PRIVATE
    Catch2::Catch2WithMain
    kg_sourcetraildb
    kg_common
    kg_model
    kg_db
    kg_index
    kg_query
    kg_out
    SQLite::SQLite3
    kg_options
)
target_compile_definitions(kg_unit_tests PRIVATE
    KG_TEST_BINARY_DIR="${CMAKE_CURRENT_BINARY_DIR}"
    KG_TEST_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}"
    # Absolute path to the fixture DB `fixture_index` (below) produces --
    # used by the [fixture]-tagged Phase 4 tests (test_bfs.cpp,
    # test_query_fixture.cpp). Deliberately NOT KG_TEST_BINARY_DIR-relative:
    # this target's own binary dir is <build>/test, one level below where
    # fixture.srctrldb actually lands (<build>/fixture.srctrldb).
    KG_FIXTURE_DB="${CMAKE_BINARY_DIR}/fixture.srctrldb"
    # Absolute path to the synthetic fixture's compile_commands.json (see
    # fixture_cdb_generate below) -- used by test_json_shape.cpp's
    # kg.index-summary/1 test, which re-runs the real kg::index::runIndex()
    # pipeline in-process (not by shelling out to the `kg` binary) against a
    # scratch output db, to exercise the actual --json summary code path.
    KG_FIXTURE_CDB="${CMAKE_BINARY_DIR}/fixture_build/compile_commands.json"
    # Absolute path to the Phase 6 hermetic CUDA fixture's database (see
    # cuda_fixture_index below) -- used by test_cuda_fixture.cpp's
    # [cuda_fixture]-tagged tests.
    KG_CUDA_FIXTURE_DB="${CMAKE_BINARY_DIR}/cuda_fixture.srctrldb"
)

# TEST_PREFIX so `ctest -R writer_smoke` (the Phase 1 verification command)
# matches the CTest-level test name, independent of how the Catch2
# TEST_CASE() itself is titled.
#
# Three registrations against the SAME binary, split by Catch2 tag
# (TEST_SPEC), following the same "TEST_PREFIX disambiguates multiple
# catch_discover_tests() calls against one target" pattern Phase 1 used for
# writer_smoke -- this is how a test that needs build/fixture.srctrldb gets
# `FIXTURES_REQUIRED FixtureDb` (so CTest always runs
# fixture_cdb_generate -> fixture_index first) while everything else stays
# fixture-independent and runs immediately.
#
# test_json_shape.cpp's cases are tagged "[fixture][json_shape]" -- the
# "[fixture]~[json_shape]" TEST_SPEC below excludes them from the
# "fixture_query." registration (they'd otherwise ALSO match plain
# "[fixture]" there, discovering/running every case twice under two
# different CTest names) so `ctest -R json_shape` cleanly finds exactly
# the third registration's tests.
#
# Phase 6's test_cuda_fixture.cpp cases are tagged "[fixture][cuda_fixture]"
# for the exact same reason, but need a DIFFERENT FIXTURES_REQUIRED
# (FixtureCudaDb, not FixtureDb -- see cuda_fixture_index below) than every
# other "[fixture]"-tagged test, so they get their own carve-out
# ("~[cuda_fixture]" on "fixture_query.") and registration, same pattern as
# json_shape's.
catch_discover_tests(kg_unit_tests
    TEST_PREFIX "writer_smoke."
    TEST_SPEC "~[fixture]")

catch_discover_tests(kg_unit_tests
    TEST_PREFIX "fixture_query."
    TEST_SPEC "[fixture]~[json_shape]~[cuda_fixture]"
    PROPERTIES FIXTURES_REQUIRED FixtureDb)

catch_discover_tests(kg_unit_tests
    TEST_PREFIX "json_shape."
    TEST_SPEC "[json_shape]"
    PROPERTIES FIXTURES_REQUIRED FixtureDb)

catch_discover_tests(kg_unit_tests
    TEST_PREFIX "cuda_fixture_query."
    TEST_SPEC "[cuda_fixture]"
    PROPERTIES FIXTURES_REQUIRED FixtureCudaDb)

# --- kg_integration_tests ------------------------------------------------
# Phase 6: the real cucs-corpus integration test. A plain hand-rolled
# main() (see test_cucs.cpp's own header comment for why it isn't a Catch2
# TEST_CASE) that must be able to `exit(77)` immediately -- before any
# Catch2 session/reporter setup could run -- if KG_CUCS_DIR's corpus isn't
# present, so CTest's SKIP_RETURN_CODE convention (below) reports it as
# SKIPPED rather than FAILED on a machine without the corpus checked out.
# Only built when KG_BUILD_INTEGRATION_TESTS is set (it is by default), so
# the target/graph shape Phase 1 locked in stays stable.
if(KG_BUILD_INTEGRATION_TESTS)
    add_executable(kg_integration_tests
        integration/test_cucs.cpp
    )
    target_link_libraries(kg_integration_tests PRIVATE
        kg_db
        kg_query
        kg_common
        kg_options
        nlohmann_json::nlohmann_json
        SQLite::SQLite3
    )
    target_compile_definitions(kg_integration_tests PRIVATE
        KG_CUCS_DIR="${KG_CUCS_DIR}"
        KG_BINARY="$<TARGET_FILE:kg>"
        KG_TEST_OUTPUT_DIR="${CMAKE_BINARY_DIR}"
    )
endif()

# cucs_integration: a SINGLE CTest test (not catch_discover_tests -- this
# binary has no Catch2 test cases to discover, see above), labeled
# "integration" so it's excluded from the default `ctest -LE integration`
# run and only picked up by `ctest -L integration`. SKIP_RETURN_CODE 77
# pairs with test_cucs.cpp's exit(77) when KG_CUCS_DIR's corpus is missing.
if(KG_BUILD_INTEGRATION_TESTS)
    add_test(NAME cucs_integration COMMAND kg_integration_tests)
    set_tests_properties(cucs_integration PROPERTIES
        LABELS "integration"
        TIMEOUT 3600
        SKIP_RETURN_CODE 77)
endif()

# --- Synthetic fixture project: CDB generation + real indexing ---------------
#
# fixture_cdb_generate configures (never builds) test/fixtures/synthetic so
# CMAKE_EXPORT_COMPILE_COMMANDS=ON produces a compile_commands.json.
# fixture_index then runs the real `kg index` pipeline against that CDB,
# producing build/fixture.srctrldb -- the database later phases' tests
# query against (via FIXTURES_REQUIRED FixtureDb).
add_test(NAME fixture_cdb_generate COMMAND ${CMAKE_COMMAND}
         -S ${CMAKE_SOURCE_DIR}/test/fixtures/synthetic
         -B ${CMAKE_BINARY_DIR}/fixture_build
         -G Ninja -DCMAKE_EXPORT_COMPILE_COMMANDS=ON)
set_tests_properties(fixture_cdb_generate PROPERTIES FIXTURES_SETUP FixtureCdb)

add_test(NAME fixture_index COMMAND $<TARGET_FILE:kg> index
         -c ${CMAKE_BINARY_DIR}/fixture_build/compile_commands.json
         -o ${CMAKE_BINARY_DIR}/fixture.srctrldb --force)
set_tests_properties(fixture_index PROPERTIES FIXTURES_REQUIRED FixtureCdb
                                              FIXTURES_SETUP FixtureDb)

# --- Phase 6: hermetic synthetic CUDA fixture --------------------------------
#
# Unlike the synthetic fixture above (a real nested CMake project configured
# at CTest time to produce a compile_commands.json), the CUDA fixture's CDB
# entry needs a specific, realistic, hand-written nvcc-shaped command line
# (see test/fixtures/cuda/compile_commands.json.in) rather than whatever a
# normal CMake/Ninja CUDA-language build would generate -- so it's simpler
# to materialize it via a single configure_file() call, done unconditionally
# at the main project's own CMake-configure time (no separate CTest
# fixture/step needed for this part, unlike fixture_cdb_generate above).
set(KG_CUDA_FIXTURE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/fixtures/cuda")
set(KG_CUDA_FIXTURE_CU_FILE "${KG_CUDA_FIXTURE_DIR}/kernels.cu")
set(KG_CUDA_FIXTURE_BUILD_DIR "${CMAKE_BINARY_DIR}/cuda_fixture")
set(KG_CUDA_FIXTURE_OUTPUT "${KG_CUDA_FIXTURE_BUILD_DIR}/kernels.cu.o")
configure_file(
    "${KG_CUDA_FIXTURE_DIR}/compile_commands.json.in"
    "${KG_CUDA_FIXTURE_BUILD_DIR}/compile_commands.json"
    @ONLY)

# cuda_fixture_index: runs the real `kg index` pipeline, forced to the
# "nowrapper" CUDA rung (`-x cuda -nocudainc -nocudalib --cuda-host-only`,
# see kg::index::cuda::synthesize()) with the hand-written fake_cuda.h stub
# force-included via --extra-arg -include --extra-arg <path> -- zero
# dependency on any real CUDA toolkit being installed (see
# docs/KNOWN_LIMITATIONS.md's Phase 6 section for the header's iteration
# log). Deliberately NOT part of any "integration"-labeled test: this is
# hermetic and must always run and always pass in the default `ctest` suite.
add_test(NAME cuda_fixture_index COMMAND $<TARGET_FILE:kg> index
         --cuda-mode nowrapper
         -c ${KG_CUDA_FIXTURE_BUILD_DIR}/compile_commands.json
         -o ${CMAKE_BINARY_DIR}/cuda_fixture.srctrldb --force
         --extra-arg -include --extra-arg ${KG_CUDA_FIXTURE_DIR}/fake_cuda.h)
set_tests_properties(cuda_fixture_index PROPERTIES FIXTURES_SETUP FixtureCudaDb)
