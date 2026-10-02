# Git-driven `kg update` acceptance test (Phase 4).
#
# Run as a CMake script (`-P`), invoked by CTest as the `update_scenario`
# test (see ../CMakeLists.txt for the exact `add_test()` registration).
# Unlike Phase 3's `--files`-only fixture tests (test_update_driver.cpp),
# this drives `kg update` against a REAL git repository with a real commit
# history -- edit/edit-header/delete/add/rename/flag-change/uncommitted-
# then-reverted scenarios -- and after every step verifies the
# incrementally-updated database is logically IDENTICAL to a full
# `kg index` re-run of the same commit (`kg dbdiff`, exit 0 == equivalent).
#
# Required variables (all passed by test/CMakeLists.txt's add_test()):
#   KG      -- absolute path to the `kg` executable
#   SRC     -- test/fixtures/synthetic (the source fixture project; never
#              modified in place -- everything happens in a throwaway copy)
#   WORK    -- scratch directory (recreated from scratch every run)
#   GIT     -- absolute path to `git`
#   SQLITE3 -- absolute path to the `sqlite3` CLI
#
# Exits 77 (CTest's SKIP_RETURN_CODE) if git/sqlite3 weren't found at
# configure time; FATAL_ERROR (CTest FAILED) on any assertion or command
# failure.

cmake_minimum_required(VERSION 3.25) # cmake_language(EXIT ...), string(JSON ...)

if(NOT DEFINED KG OR NOT DEFINED SRC OR NOT DEFINED WORK)
    message(FATAL_ERROR "update_scenario.cmake: KG, SRC and WORK must all be set")
endif()

if(NOT DEFINED GIT OR GIT STREQUAL "" OR NOT EXISTS "${GIT}")
    message("update_scenario: git not found -- skipping")
    cmake_language(EXIT 77)
endif()

if(NOT DEFINED SQLITE3 OR SQLITE3 STREQUAL "" OR NOT EXISTS "${SQLITE3}")
    message("update_scenario: sqlite3 not found -- skipping")
    cmake_language(EXIT 77)
endif()

# ------------------------------------------------------------------------
# Helpers
# ------------------------------------------------------------------------

# kg_run(<desc> <workdir> <expected-rc> <cmd...>) -- runs a command, prints
# its captured stdout/stderr and FATAL_ERRORs if the exit code isn't the
# expected one. Sets KG_OUT / KG_ERR in the caller's scope.
function(kg_run desc workdir expected_rc)
    set(cmd ${ARGN})
    execute_process(
        COMMAND ${cmd}
        WORKING_DIRECTORY "${workdir}"
        RESULT_VARIABLE rc
        OUTPUT_VARIABLE out
        ERROR_VARIABLE err)
    if(NOT rc EQUAL expected_rc)
        message(FATAL_ERROR
            "${desc}\n  command: ${cmd}\n  expected exit ${expected_rc}, got ${rc}\n"
            "--- stdout ---\n${out}\n--- stderr ---\n${err}")
    endif()
    set(KG_OUT "${out}" PARENT_SCOPE)
    set(KG_ERR "${err}" PARENT_SCOPE)
endfunction()

# sqlite_scalar(<outvar> <dbpath> <sql>) -- runs a single-column sqlite3
# query and stores the (trimmed) first row's value in <outvar>.
function(sqlite_scalar outvar dbpath sql)
    execute_process(
        COMMAND "${SQLITE3}" "${dbpath}" "${sql}"
        RESULT_VARIABLE rc
        OUTPUT_VARIABLE out
        ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "sqlite3 query failed on ${dbpath}: ${sql}\n${err}")
    endif()
    string(STRIP "${out}" out)
    set(${outvar} "${out}" PARENT_SCOPE)
endfunction()

# assert_eq(<desc> <a> <b>)
function(assert_eq desc a b)
    if(NOT "${a}" STREQUAL "${b}")
        message(FATAL_ERROR "assertion failed: ${desc}\n  got:      '${a}'\n  expected: '${b}'")
    endif()
endfunction()

function(assert_ge desc a b)
    if(${a} LESS ${b})
        message(FATAL_ERROR "assertion failed: ${desc}\n  got:      ${a}\n  expected: >= ${b}")
    endif()
endfunction()

# json_field(<outvar> <json-text> <path...>) -- thin wrapper over
# string(JSON GET ...) that FATAL_ERRORs with the json text on failure
# (instead of string(JSON)'s own less-obviously-attributable error).
function(json_field outvar json_text)
    string(JSON value ERROR_VARIABLE jerr GET "${json_text}" ${ARGN})
    if(jerr)
        message(FATAL_ERROR "json_field(${ARGN}) failed: ${jerr}\n--- json ---\n${json_text}")
    endif()
    set(${outvar} "${value}" PARENT_SCOPE)
endfunction()

# git_commit_all(<workdir> <message>)
function(git_commit_all workdir message)
    execute_process(COMMAND "${GIT}" add -A WORKING_DIRECTORY "${workdir}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "git add -A failed: ${err}")
    endif()
    execute_process(
        COMMAND "${GIT}" -c user.name=kg -c user.email=kg@test commit -q -m "${message}"
        WORKING_DIRECTORY "${workdir}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "git commit failed: ${out}\n${err}")
    endif()
endfunction()

function(git_head outvar workdir)
    execute_process(COMMAND "${GIT}" rev-parse HEAD WORKING_DIRECTORY "${workdir}"
        OUTPUT_VARIABLE out RESULT_VARIABLE rc ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "git rev-parse HEAD failed: ${err}")
    endif()
    string(STRIP "${out}" out)
    set(${outvar} "${out}" PARENT_SCOPE)
endfunction()

# reconfigure(): re-run cmake configure on WORK/build from WORK/repo so
# compile_commands.json reflects whatever add/delete/rename just happened
# on disk (a plain `cmake --build` wouldn't re-glob without this).
function(reconfigure)
    kg_run("reconfigure WORK/build" "${WORK}" 0
        "${CMAKE_COMMAND}" -S "${WORK}/repo" -B "${WORK}/build" -G Ninja
        -DCMAKE_EXPORT_COMPILE_COMMANDS=ON)
endfunction()

set(CDB "${WORK}/build/compile_commands.json")

# kg_index(<outdb>) -- a full `kg index --force` of the CURRENT repo state,
# for later dbdiff comparison against the incrementally-updated database.
function(kg_index outdb)
    kg_run("kg index -> ${outdb}" "${WORK}" 0
        "${KG}" index -c "${CDB}" -o "${outdb}" --repo "${WORK}/repo" --force)
endfunction()

# kg_dbdiff(<a> <b>) -- must be logically equivalent (exit 0); prints both
# sides' output on mismatch.
function(kg_dbdiff a b)
    execute_process(
        COMMAND "${KG}" dbdiff "${a}" "${b}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "kg dbdiff ${a} ${b} : expected equivalent (exit 0), got ${rc}\n${out}\n${err}")
    endif()
endfunction()

# kg_update_json(<jsonvar> <expected-rc> <extra-args...>) -- runs
# `kg update -d WORK/inc.srctrldb --json <extra-args...>`, checks the exit
# code, and stores the parsed JSON text in <jsonvar> (empty string if the
# exit code doesn't produce JSON, e.g. a refusal).
function(kg_update_json jsonvar expected_rc)
    set(extra ${ARGN})
    kg_run("kg update" "${WORK}" "${expected_rc}"
        "${KG}" update -d "${WORK}/inc.srctrldb" -c "${CDB}" --repo "${WORK}/repo" --json ${extra})
    set(${jsonvar} "${KG_OUT}" PARENT_SCOPE)
endfunction()

# kg_notes_json(<jsonvar> <expected-rc> <verb> <extra-args...>) -- runs
# `kg notes <verb> -d WORK/inc.srctrldb --json <extra-args...>`, checks the
# exit code, stores the parsed JSON text in <jsonvar>.
function(kg_notes_json jsonvar expected_rc verb)
    set(extra ${ARGN})
    kg_run("kg notes ${verb}" "${WORK}" "${expected_rc}"
        "${KG}" notes "${verb}" -d "${WORK}/inc.srctrldb" --json ${extra})
    set(${jsonvar} "${KG_OUT}" PARENT_SCOPE)
endfunction()

# ------------------------------------------------------------------------
# Step 1: prepare a real git repo, rewritten to glob its sources
# ------------------------------------------------------------------------

message(STATUS "update_scenario: preparing ${WORK}/repo")
file(REMOVE_RECURSE "${WORK}")
file(MAKE_DIRECTORY "${WORK}")
file(COPY "${SRC}/" DESTINATION "${WORK}/repo")

# Rewritten CMakeLists.txt: file(GLOB_RECURSE ...) for fixture_lib's
# sources (everything under src/ except tu_a.cpp/tu_b.cpp, which keep their
# own standalone executables) so add/delete/rename need no CMakeLists.txt
# edits -- same targets/flags as the original, just glob-driven.
file(WRITE "${WORK}/repo/CMakeLists.txt" "\
# Rewritten by test/update/update_scenario.cmake from the original
# test/fixtures/synthetic/CMakeLists.txt -- GLOB_RECURSE instead of an
# explicit source list so add/delete/rename scenarios need no edits here.
cmake_minimum_required(VERSION 3.20)
project(kg_fixture CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

include_directories(\${CMAKE_CURRENT_SOURCE_DIR}/include)

file(GLOB_RECURSE FIXTURE_ALL_SOURCES CONFIGURE_DEPENDS \${CMAKE_CURRENT_SOURCE_DIR}/src/*.cpp)
list(REMOVE_ITEM FIXTURE_ALL_SOURCES
    \${CMAKE_CURRENT_SOURCE_DIR}/src/tu_a.cpp
    \${CMAKE_CURRENT_SOURCE_DIR}/src/tu_b.cpp)

add_library(fixture_lib STATIC \${FIXTURE_ALL_SOURCES})

add_executable(tu_a src/tu_a.cpp)
add_executable(tu_b src/tu_b.cpp)
")

execute_process(COMMAND "${GIT}" init -q WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git init failed: ${err}")
endif()
git_commit_all("${WORK}/repo" "initial commit")
git_head(COMMIT_0 "${WORK}/repo")
message(STATUS "update_scenario: commit 0 = ${COMMIT_0}")

# ------------------------------------------------------------------------
# Step 2: configure WORK/build, sanity-check the glob-driven CDB
# ------------------------------------------------------------------------

reconfigure()
if(NOT EXISTS "${CDB}")
    message(FATAL_ERROR "compile_commands.json wasn't produced at ${CDB}")
endif()

file(READ "${CDB}" CDB_TEXT)
string(JSON CDB_LEN LENGTH "${CDB_TEXT}")
set(EXPECTED_TU_COUNT 6)
if(NOT CDB_LEN EQUAL EXPECTED_TU_COUNT)
    message(FATAL_ERROR "expected ${EXPECTED_TU_COUNT} compile_commands.json entries after the GLOB_RECURSE rewrite, got ${CDB_LEN}:\n${CDB_TEXT}")
endif()
set(EXPECTED_TU_NAMES chain.cpp ns.cpp overloads.cpp templates.cpp tu_a.cpp tu_b.cpp)
math(EXPR CDB_LAST "${CDB_LEN} - 1")
set(CDB_FILES "")
foreach(i RANGE ${CDB_LAST})
    json_field(f "${CDB_TEXT}" ${i} file)
    get_filename_component(fname "${f}" NAME)
    list(APPEND CDB_FILES "${fname}")
endforeach()
foreach(expected ${EXPECTED_TU_NAMES})
    list(FIND CDB_FILES "${expected}" idx)
    if(idx LESS 0)
        message(FATAL_ERROR "GLOB_RECURSE-driven compile_commands.json is missing ${expected}; got: ${CDB_FILES}")
    endif()
endforeach()
message(STATUS "update_scenario: initial CDB has the expected ${EXPECTED_TU_COUNT} entries: ${CDB_FILES}")

# ------------------------------------------------------------------------
# Step 3: first full index, baseline node ids
# ------------------------------------------------------------------------

kg_index("${WORK}/inc.srctrldb")

sqlite_scalar(META_COMMIT "${WORK}/inc.srctrldb" "select value from meta where key='kg_repo_commit'")
assert_eq("meta.kg_repo_commit after initial index" "${META_COMMIT}" "${COMMIT_0}")

sqlite_scalar(SHAPE_ID_0 "${WORK}/inc.srctrldb"
    "select id from node where type=128 and serialized_name like '%Shape%' order by id limit 1")
if(SHAPE_ID_0 STREQUAL "")
    message(FATAL_ERROR "couldn't find fixture::Shape's node id in the freshly-indexed database")
endif()
message(STATUS "update_scenario: fixture::Shape node id = ${SHAPE_ID_0}")

# ------------------------------------------------------------------------
# Step 4a: edit src/chain.cpp's body (add level5(), called from level4())
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step a -- edit chain.cpp")
file(READ "${WORK}/repo/src/chain.cpp" CHAIN_TEXT)
string(REPLACE
    "int level4()\n{\n    return 4;\n}"
    "int level5()\n{\n    return 5;\n}\n\nint level4()\n{\n    return level5() - 1;\n}"
    CHAIN_TEXT "${CHAIN_TEXT}")
file(WRITE "${WORK}/repo/src/chain.cpp" "${CHAIN_TEXT}")
git_commit_all("${WORK}/repo" "a: edit chain.cpp body (add level5())")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files cleared)
assert_eq("step a: files.cleared" "${v}" "1")

sqlite_scalar(SHAPE_ID_AFTER_A "${WORK}/inc.srctrldb"
    "select id from node where type=128 and serialized_name like '%Shape%' order by id limit 1")
assert_eq("step a: fixture::Shape node id unchanged (chain.cpp doesn't touch shapes.hpp)" "${SHAPE_ID_AFTER_A}" "${SHAPE_ID_0}")

sqlite_scalar(LEVEL0_ID_BASE "${WORK}/inc.srctrldb"
    "select id from node where serialized_name like '%level0%' order by id limit 1")
if(LEVEL0_ID_BASE STREQUAL "")
    message(FATAL_ERROR "couldn't find level0's node id after step a")
endif()
message(STATUS "update_scenario: level0 node id (post-step-a baseline) = ${LEVEL0_ID_BASE}")

kg_index("${WORK}/full_a.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_a.srctrldb")

# ------------------------------------------------------------------------
# Step 4b: edit include/shapes.hpp (add an inline method to Shape)
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step b -- edit shapes.hpp")
file(READ "${WORK}/repo/include/shapes.hpp" SHAPES_TEXT)
string(REPLACE
    "virtual double area() const = 0;"
    "virtual double area() const = 0;\n    virtual double perimeterHint() const { return 0.0; }"
    SHAPES_TEXT "${SHAPES_TEXT}")
file(WRITE "${WORK}/repo/include/shapes.hpp" "${SHAPES_TEXT}")
git_commit_all("${WORK}/repo" "b: edit shapes.hpp (add Shape::perimeterHint())")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files cleared)
assert_ge("step b: files.cleared" "${v}" "3")

sqlite_scalar(LEVEL0_ID "${WORK}/inc.srctrldb" "select id from node where serialized_name like '%level0%' order by id limit 1")
assert_eq("step b: level0 node id stable (chain.cpp untouched)" "${LEVEL0_ID}" "${LEVEL0_ID_BASE}")

kg_index("${WORK}/full_b.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_b.srctrldb")

# ------------------------------------------------------------------------
# Step 4b2: class Square -> struct Square (kind-change regression, see
# docs/KNOWN_LIMITATIONS.md's Phase 8 section). Square's node.type was
# already CLASS from the baseline full index; node.type is written
# upgrade-only (see SrctrldbBackend::flushPending()'s kUpdateNodeTypeSql),
# so without Invalidator's step-4b node.type reset (UPDATE node SET
# type=UNKNOWN for every node in kg_lost), this genuine source edit would
# silently NOT take effect on `kg update` -- CLASS (rank 2) would never be
# "downgraded" back to STRUCT (also rank 2, but a different node-kind int)
# by the re-index's own upgrade-only write, unlike a fresh `kg index` of
# the same edited source, which sees only STRUCT and writes STRUCT from
# scratch. This is exactly the class of bug the reset exists to prevent.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step b2 -- class Square -> struct Square")
file(READ "${WORK}/repo/include/shapes.hpp" SHAPES_TEXT)
string(REPLACE
    "class Square : public Polygon, public Colored"
    "struct Square : public Polygon, public Colored"
    SHAPES_TEXT "${SHAPES_TEXT}")
file(WRITE "${WORK}/repo/include/shapes.hpp" "${SHAPES_TEXT}")
git_commit_all("${WORK}/repo" "b2: class Square -> struct Square")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files cleared)
assert_ge("step b2: files.cleared" "${v}" "3")

# Match Square's own node precisely by its exact leaf NameElement encoding
# (char(9) || 'mSquare' || char(9) || 's' || char(9) || 'p' -- prefix and
# postfix both empty, i.e. the bare record itself, NOT e.g. its
# `Square(double)` constructor, which has a non-empty postfix).
# '_' is sqlite LIKE's single-char wildcard, matching whichever of 'm'
# (first/root NameElement) or 'n' (a later one -- Square is nested under
# the `fixture` namespace, so 'n' in practice) precedes the name.
sqlite_scalar(SQUARE_TYPE "${WORK}/inc.srctrldb"
    "select type from node where serialized_name like ('%' || char(9) || '_Square' || char(9) || 's' || char(9) || 'p')")
# NodeKind::STRUCT's int (1 << 6 = 64, see third_party/sourcetraildb/include/NodeKind.h) --
# a fresh index of the edited source (struct Square) writes exactly this.
assert_eq("step b2: Square's node.type reflects the struct edit, not the stale CLASS kind" "${SQUARE_TYPE}" "64")

kg_index("${WORK}/full_b2.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_b2.srctrldb")

# ------------------------------------------------------------------------
# Step 4c: git rm src/overloads.cpp
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step c -- delete overloads.cpp")
execute_process(COMMAND "${GIT}" rm -q src/overloads.cpp WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git rm src/overloads.cpp failed: ${err}")
endif()
git_commit_all("${WORK}/repo" "c: delete overloads.cpp")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files deleted)
assert_eq("step c: files.deleted" "${v}" "1")

sqlite_scalar(LEVEL0_ID "${WORK}/inc.srctrldb" "select id from node where serialized_name like '%level0%' order by id limit 1")
assert_eq("step c: level0 node id stable" "${LEVEL0_ID}" "${LEVEL0_ID_BASE}")

kg_index("${WORK}/full_c.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_c.srctrldb")

# ------------------------------------------------------------------------
# Step 4d: add src/new_tu.cpp (includes shapes.hpp, uses Square)
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step d -- add new_tu.cpp")
file(WRITE "${WORK}/repo/src/new_tu.cpp" "\
#include \"shapes.hpp\"

int useSquareFromNewTu()
{
    fixture::Square sq(2.0);
    return static_cast<int>(sq.area());
}
")
git_commit_all("${WORK}/repo" "d: add new_tu.cpp")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" tus new)
assert_eq("step d: tus.new" "${v}" "1")

sqlite_scalar(LEVEL0_ID "${WORK}/inc.srctrldb" "select id from node where serialized_name like '%level0%' order by id limit 1")
assert_eq("step d: level0 node id stable" "${LEVEL0_ID}" "${LEVEL0_ID_BASE}")

kg_index("${WORK}/full_d.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_d.srctrldb")

# ------------------------------------------------------------------------
# Step 4e: git mv src/ns.cpp src/ns2.cpp
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step e -- rename ns.cpp -> ns2.cpp")
execute_process(COMMAND "${GIT}" mv src/ns.cpp src/ns2.cpp WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git mv src/ns.cpp src/ns2.cpp failed: ${err}")
endif()
git_commit_all("${WORK}/repo" "e: rename ns.cpp -> ns2.cpp")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files renamed)
assert_eq("step e: files.renamed" "${v}" "1")

sqlite_scalar(LEVEL0_ID "${WORK}/inc.srctrldb" "select id from node where serialized_name like '%level0%' order by id limit 1")
assert_eq("step e: level0 node id stable" "${LEVEL0_ID}" "${LEVEL0_ID_BASE}")

kg_index("${WORK}/full_e.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_e.srctrldb")

# ------------------------------------------------------------------------
# Step 4f: compiler-flag-only change (add_compile_definitions)
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step f -- flag change (KG_SCENARIO_FLAG=1)")
file(APPEND "${WORK}/repo/CMakeLists.txt" "\nadd_compile_definitions(KG_SCENARIO_FLAG=1)\n")
git_commit_all("${WORK}/repo" "f: add_compile_definitions(KG_SCENARIO_FLAG=1)")
reconfigure()

# The current CDB (6 entries again: chain/ns2/templates/new_tu/tu_a/tu_b --
# overloads.cpp is gone, ns.cpp->ns2.cpp, new_tu.cpp added) should ALL be
# reported as flag-changed, since the flag applies project-wide.
file(READ "${CDB}" CDB_TEXT_F)
string(JSON CDB_LEN_F LENGTH "${CDB_TEXT_F}")

kg_update_json(J 0)
json_field(v "${J}" files flag_changed)
assert_eq("step f: files.flag_changed == current TU count" "${v}" "${CDB_LEN_F}")

# NOTE: level0's node id is deliberately NOT asserted stable across this
# step -- unlike b/c/d/e, a flag change re-indexes EVERY TU, including
# chain.cpp itself (chain.cpp's own compile flags changed too), so its
# symbols (including level0) are fully cleared and re-created with fresh
# ids. This is expected per docs/KNOWN_LIMITATIONS.md's Phase 3 note on
# why GC must run to completion before re-indexing (ids are not preserved
# across a real clear+reindex of their own declaring file).
kg_index("${WORK}/full_f.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_f.srctrldb")

sqlite_scalar(LEVEL0_ID_BASE "${WORK}/inc.srctrldb" "select id from node where serialized_name like '%level0%' order by id limit 1")
message(STATUS "update_scenario: level0 node id (post-step-f, new baseline) = ${LEVEL0_ID_BASE}")

# ------------------------------------------------------------------------
# Step 4g: uncommitted change, then revert
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step g -- uncommitted chain.cpp edit")
file(READ "${WORK}/repo/src/chain.cpp" CHAIN_TEXT_G)
string(REPLACE
    "int runChain()"
    "int extraHelperG() { return 42; }\n\nint runChain()"
    CHAIN_TEXT_G "${CHAIN_TEXT_G}")
file(WRITE "${WORK}/repo/src/chain.cpp" "${CHAIN_TEXT_G}")
# Deliberately NOT committed.
reconfigure()
git_head(HEAD_AT_G "${WORK}/repo")

kg_update_json(J 0)
json_field(v "${J}" files changed)
assert_eq("step g (dirty): files.changed" "${v}" "1")
json_field(v "${J}" repo to)
assert_eq("step g (dirty): repo.to == HEAD (uncommitted change tracked via dirty paths, not a new commit)" "${v}" "${HEAD_AT_G}")

kg_index("${WORK}/full_g1.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_g1.srctrldb")

message(STATUS "update_scenario: step g -- revert the uncommitted edit")
execute_process(COMMAND "${GIT}" checkout -q -- src/chain.cpp WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git checkout -- src/chain.cpp failed: ${err}")
endif()
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" files changed)
assert_eq("step g (reverted): files.changed (still re-indexed, via kg_dirty_paths)" "${v}" "1")

kg_index("${WORK}/full_g2.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_g2.srctrldb")

# ------------------------------------------------------------------------
# Step 4g2: delete a HEADER (include/macros.hpp) that's still included by a
# TU (src/ns2.cpp, post step-e rename) -- a deleted header alone (never a
# kg_tu row itself) used to be invisible to the "nothing to do?" check
# (which only looked at match.deleted, not the deletedSeed set that
# actually captures header deletions too), so `kg update` would wrongly
# report exit 5 and leave the header (and its includer) uncleared.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step g2 -- delete macros.hpp (a header still included by ns2.cpp)")
file(READ "${WORK}/repo/src/ns2.cpp" NS2_TEXT)
string(REPLACE "#include \"macros.hpp\"\n" "" NS2_TEXT "${NS2_TEXT}")
string(REPLACE "int gGlobalCounter = FIXTURE_VERSION;" "int gGlobalCounter = 3;" NS2_TEXT "${NS2_TEXT}")
string(REPLACE "int squared = FIXTURE_SQUARE(li);" "int squared = (li) * (li);" NS2_TEXT "${NS2_TEXT}")
file(WRITE "${WORK}/repo/src/ns2.cpp" "${NS2_TEXT}")
execute_process(COMMAND "${GIT}" rm -q include/macros.hpp WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git rm include/macros.hpp failed: ${err}")
endif()
git_commit_all("${WORK}/repo" "g2: delete macros.hpp, drop its use from ns2.cpp")
reconfigure()

kg_update_json(J 0)
# macros.hpp is a HEADER, never a kg_tu row -- `files.deleted` (from
# match.deleted, which only ever tracks TU deletions) legitimately stays 0
# for a header-only deletion; what matters is that it's still cleared (via
# the deletedSeed path -- see UpdateDriver.cpp step 4's emptiness check)
# rather than silently ignored (the exact bug this step regression-tests).
json_field(v "${J}" files deleted)
assert_eq("step g2: files.deleted (headers never count as TU deletions)" "${v}" "0")
json_field(v "${J}" files cleared)
assert_ge("step g2: files.cleared (macros.hpp + its includer ns2.cpp, at least)" "${v}" "2")

kg_index("${WORK}/full_g2b.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_g2b.srctrldb")

# ------------------------------------------------------------------------
# Step 4h: nothing changed -> exit 5
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step h -- nothing changed")
kg_update_json(J 5)

# ------------------------------------------------------------------------
# Step 5: --in-place on a final edit
# ------------------------------------------------------------------------

message(STATUS "update_scenario: step (in-place) -- final edit, --in-place update")
file(READ "${WORK}/repo/src/chain.cpp" CHAIN_TEXT_IP)
string(REPLACE
    "int runChain()"
    "int extraHelperInPlace() { return 7; }\n\nint runChain()"
    CHAIN_TEXT_IP "${CHAIN_TEXT_IP}")
file(WRITE "${WORK}/repo/src/chain.cpp" "${CHAIN_TEXT_IP}")
git_commit_all("${WORK}/repo" "in-place: another chain.cpp edit")
reconfigure()

kg_update_json(J 0 --in-place)

sqlite_scalar(IN_PROGRESS "${WORK}/inc.srctrldb" "select value from meta where key='kg_update_in_progress'")
assert_eq("kg_update_in_progress absent after a successful --in-place update" "${IN_PROGRESS}" "")

kg_index("${WORK}/full_inplace.srctrldb")
kg_dbdiff("${WORK}/inc.srctrldb" "${WORK}/full_inplace.srctrldb")

# ------------------------------------------------------------------------
# Step (notes): Phase 3 of docs/NOTES_DESIGN.md -- scaffold/fill/sync/commit
# notes for chain.cpp only (--file, not --all, so editing an unrelated
# function later provably can't touch any note), then drive `kg update`
# through edit/edit-elsewhere/accept/rename/delete scenarios, asserting via
# `kg update --json`'s `notes` object and `kg notes status/sync --json`.
# ------------------------------------------------------------------------

set(NOTES_DIR "${WORK}/repo/notes")

message(STATUS "update_scenario: notes step 1 -- scaffold + fill + sync + commit")
kg_run("kg notes scaffold" "${WORK}" 0
    "${KG}" notes scaffold -d "${WORK}/inc.srctrldb" --notes-dir "${NOTES_DIR}" --file "src/chain.cpp")
if(NOT EXISTS "${NOTES_DIR}/src/chain.cpp.md")
    message(FATAL_ERROR "update_scenario: notes scaffold didn't create ${NOTES_DIR}/src/chain.cpp.md")
endif()

if(DEFINED SCRIPTS AND NOT SCRIPTS STREQUAL "")
    kg_run("kg-notes-fake --fill-existing" "${WORK}" 0
        python3 "${SCRIPTS}/kg-notes-fake.py" --fill-existing "${NOTES_DIR}" --seed 5 --fill-ratio 0.6)
endif()

kg_notes_json(J 0 sync --notes-dir "${NOTES_DIR}")
json_field(v "${J}" counts stale)
assert_eq("notes step 1: freshly-synced notes have 0 stale" "${v}" "0")

git_commit_all("${WORK}/repo" "notes: scaffold + fill chain.cpp notes")

# ------------------------------------------------------------------------
# Step (notes-a): edit level1's BODY -> exactly level1 goes stale, and
# `kg update`'s summary reports notes.newly_stale == 1.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: notes step a -- edit level1's body")
file(READ "${WORK}/repo/src/chain.cpp" CHAIN_TEXT_NA)
string(REPLACE
    "int level1()\n{\n    return level2() + 1;\n}"
    "int level1()\n{\n    return level2() + 2;\n}"
    CHAIN_TEXT_NA "${CHAIN_TEXT_NA}")
file(WRITE "${WORK}/repo/src/chain.cpp" "${CHAIN_TEXT_NA}")
git_commit_all("${WORK}/repo" "notes-a: edit level1 body")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" notes newly_stale)
assert_eq("notes step a: exactly 1 newly-stale note (level1)" "${v}" "1")

kg_notes_json(J 0 status --stale)
string(JSON STALE_LEN LENGTH "${J}" stale)
assert_eq("notes step a: exactly 1 stale note listed" "${STALE_LEN}" "1")
json_field(v "${J}" stale 0)
string(FIND "${v}" "level1" LEVEL1_POS)
if(LEVEL1_POS LESS 0)
    message(FATAL_ERROR "notes step a: expected the stale note to be level1, got: ${v}")
endif()

# ------------------------------------------------------------------------
# Step (notes-b): edit a DIFFERENT (unnoted -- shapes.hpp was never
# scaffolded, only chain.cpp was) function's body -> level1 stays the only
# stale note, notes.newly_stale == 0.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: notes step b -- edit an unnoted function elsewhere")
file(READ "${WORK}/repo/include/shapes.hpp" SHAPES_TEXT_NB)
string(REPLACE
    "virtual double perimeterHint() const { return 0.0; }"
    "virtual double perimeterHint() const { return 1.0; }"
    SHAPES_TEXT_NB "${SHAPES_TEXT_NB}")
file(WRITE "${WORK}/repo/include/shapes.hpp" "${SHAPES_TEXT_NB}")
git_commit_all("${WORK}/repo" "notes-b: edit an unnoted function (Shape::perimeterHint)")
reconfigure()

kg_update_json(J 0)
json_field(v "${J}" notes newly_stale)
assert_eq("notes step b: nothing newly stale (the edited function has no note)" "${v}" "0")

kg_notes_json(J 0 status --stale)
string(JSON STALE_LEN LENGTH "${J}" stale)
assert_eq("notes step b: level1 is STILL the only stale note" "${STALE_LEN}" "1")

# ------------------------------------------------------------------------
# Step (notes-c): `kg notes accept level1` -> stale clears; the markdown's
# kg-hash changes but everything else in the file is byte-identical.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: notes step c -- accept level1")
file(READ "${NOTES_DIR}/src/chain.cpp.md" CHAIN_MD_BEFORE)

kg_notes_json(J 0 accept level1 --notes-dir "${NOTES_DIR}")
json_field(v "${J}" accepted)
string(JSON ACCEPTED_LEN LENGTH "${J}" accepted)
assert_eq("notes step c: exactly 1 note accepted" "${ACCEPTED_LEN}" "1")
string(JSON REFUSED_LEN LENGTH "${J}" refused_files)
assert_eq("notes step c: no files refused (canonical form)" "${REFUSED_LEN}" "0")

file(READ "${NOTES_DIR}/src/chain.cpp.md" CHAIN_MD_AFTER)
if(CHAIN_MD_BEFORE STREQUAL CHAIN_MD_AFTER)
    message(FATAL_ERROR "notes step c: accept should have changed level1's kg-hash comment")
endif()
string(REGEX REPLACE "kg-hash: [0-9a-f]+" "kg-hash: X" NORM_BEFORE "${CHAIN_MD_BEFORE}")
string(REGEX REPLACE "kg-commit: [0-9a-f]+" "kg-commit: X" NORM_BEFORE "${NORM_BEFORE}")
string(REGEX REPLACE "kg-hash: [0-9a-f]+" "kg-hash: X" NORM_AFTER "${CHAIN_MD_AFTER}")
string(REGEX REPLACE "kg-commit: [0-9a-f]+" "kg-commit: X" NORM_AFTER "${NORM_AFTER}")
assert_eq("notes step c: accept only rewrites kg-hash/kg-commit comments -- everything else byte-identical" "${NORM_BEFORE}" "${NORM_AFTER}")

kg_notes_json(J 0 status --stale)
string(JSON STALE_LEN LENGTH "${J}" stale)
assert_eq("notes step c: 0 stale notes after accepting level1" "${STALE_LEN}" "0")

# ------------------------------------------------------------------------
# Step (notes-d): `git mv` the noted source file -> its notes file is
# reported as an "orphaned-file" problem.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: notes step d -- git mv chain.cpp -> chain_renamed.cpp")
execute_process(COMMAND "${GIT}" mv src/chain.cpp src/chain_renamed.cpp WORKING_DIRECTORY "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git mv src/chain.cpp src/chain_renamed.cpp failed: ${err}")
endif()
git_commit_all("${WORK}/repo" "notes-d: rename chain.cpp -> chain_renamed.cpp")
reconfigure()

kg_update_json(J 0)

kg_notes_json(J 0 sync --notes-dir "${NOTES_DIR}")
json_field(v "${J}" counts orphaned_files)
assert_ge("notes step d: chain.cpp.md is reported orphaned-file" "${v}" "1")

set(FOUND_ORPHAN FALSE)
string(JSON PROBLEMS_LEN LENGTH "${J}" problems)
if(PROBLEMS_LEN GREATER 0)
    math(EXPR PROBLEMS_LAST "${PROBLEMS_LEN} - 1")
    foreach(i RANGE ${PROBLEMS_LAST})
        json_field(pkind "${J}" problems ${i} kind)
        if(pkind STREQUAL "orphaned-file")
            set(FOUND_ORPHAN TRUE)
        endif()
    endforeach()
endif()
if(NOT FOUND_ORPHAN)
    message(FATAL_ERROR "notes step d: expected an 'orphaned-file' problem in: ${J}")
endif()

# ------------------------------------------------------------------------
# Step (notes-e): delete a noted function (factorial, and its one call site
# in runChain so the TU still compiles) -> its block becomes "unmatched"
# after `kg update` + `kg notes sync`.
# ------------------------------------------------------------------------

message(STATUS "update_scenario: notes step e -- delete factorial()")
file(READ "${WORK}/repo/src/chain_renamed.cpp" CHAIN_TEXT_NE)
string(REPLACE
    "int factorial(int n)\n{\n    if (n <= 1)\n    {\n        return 1;\n    }\n    return n * factorial(n - 1);\n}\n\n"
    ""
    CHAIN_TEXT_NE "${CHAIN_TEXT_NE}")
string(REPLACE
    "return level0() + factorial(5) + ping(4);"
    "return level0() + ping(4);"
    CHAIN_TEXT_NE "${CHAIN_TEXT_NE}")
file(WRITE "${WORK}/repo/src/chain_renamed.cpp" "${CHAIN_TEXT_NE}")
git_commit_all("${WORK}/repo" "notes-e: delete factorial()")
reconfigure()

kg_update_json(J 0)

kg_notes_json(J 0 sync --notes-dir "${NOTES_DIR}")
json_field(v "${J}" counts unmatched)
assert_ge("notes step e: factorial's block is now unmatched" "${v}" "1")

message(STATUS "update_scenario: ALL STEPS PASSED")
