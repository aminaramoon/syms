# Cross-worktree proof for `kg notes` (Phase 4 of docs/NOTES_DESIGN.md).
#
# Run as a CMake script (`-P`), invoked by CTest as the `notes_cross_worktree`
# test (see ../CMakeLists.txt for the exact `add_test()` registration).
#
# The whole point of docs/NOTES_DESIGN.md's design ("Markdown in git is the
# source of truth... The .srctrldb only holds a derived cache") is that
# knowledge written into notes travels with git, INDEPENDENT of any
# particular `.srctrldb`. This script proves it end to end:
#
#   1. Build a real git repo (worktree A) from test/fixtures/synthetic,
#      `kg index` it, scaffold+fill+sync notes for every symbol, commit the
#      notes into git.
#   2. `git clone` worktree A to a SEPARATE worktree B (a different
#      filesystem path).
#   3. Independently `kg index` B into its OWN, separate .srctrldb (which
#      will have completely different node/edge ids, and different absolute
#      file paths, from A's).
#   4. `kg notes sync` in B, against B's own clone of the notes/ directory
#      (same committed markdown bytes, since it was `git clone`d).
#   5. Assert `kg dbdiff A B --only note` is EMPTY (0 differences) even
#      though a plain `kg dbdiff A B` (no --only) is very much NOT empty
#      (different node/edge/location rows -- different worktree paths) --
#      the notes cache is keyed by serialized_name, not path or id, so it's
#      identical across the two independently-built databases.
#   6. Assert `kg query context 'level2' --json` returns the SAME `notes`
#      objects (by qualified_name) in both A and B -- notes travel with git,
#      independent of the DB.
#
# Required variables (all passed by test/CMakeLists.txt's add_test()):
#   KG      -- absolute path to the `kg` executable
#   SRC     -- test/fixtures/synthetic (the source fixture project; never
#              modified in place -- everything happens in a throwaway copy)
#   WORK    -- scratch directory (recreated from scratch every run)
#   GIT     -- absolute path to `git`
#   SCRIPTS -- absolute path to the repo's scripts/ dir (kg-notes-fake.py)
#
# Exits 77 (CTest's SKIP_RETURN_CODE) if git wasn't found at configure time;
# FATAL_ERROR (CTest FAILED) on any assertion or command failure.

cmake_minimum_required(VERSION 3.25) # cmake_language(EXIT ...), string(JSON ...)

if(NOT DEFINED KG OR NOT DEFINED SRC OR NOT DEFINED WORK)
    message(FATAL_ERROR "notes_cross_worktree.cmake: KG, SRC and WORK must all be set")
endif()

if(NOT DEFINED GIT OR GIT STREQUAL "" OR NOT EXISTS "${GIT}")
    message("notes_cross_worktree: git not found -- skipping")
    cmake_language(EXIT 77)
endif()

# ------------------------------------------------------------------------
# Helpers (same conventions as update_scenario.cmake)
# ------------------------------------------------------------------------

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

function(assert_eq desc a b)
    if(NOT "${a}" STREQUAL "${b}")
        message(FATAL_ERROR "assertion failed: ${desc}\n  got:      '${a}'\n  expected: '${b}'")
    endif()
endfunction()

function(json_field outvar json_text)
    string(JSON value ERROR_VARIABLE jerr GET "${json_text}" ${ARGN})
    if(jerr)
        message(FATAL_ERROR "json_field(${ARGN}) failed: ${jerr}\n--- json ---\n${json_text}")
    endif()
    set(${outvar} "${value}" PARENT_SCOPE)
endfunction()

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

# reconfigure(<repoDir> <buildDir>): (re-)run cmake configure so
# compile_commands.json reflects the given source tree.
function(reconfigure repoDir buildDir)
    kg_run("reconfigure ${buildDir}" "${WORK}" 0
        "${CMAKE_COMMAND}" -S "${repoDir}" -B "${buildDir}" -G Ninja
        -DCMAKE_EXPORT_COMPILE_COMMANDS=ON)
endfunction()

# ------------------------------------------------------------------------
# Step 1: worktree A -- a real git repo from the synthetic fixture
# ------------------------------------------------------------------------

message(STATUS "notes_cross_worktree: preparing ${WORK}/a")
file(REMOVE_RECURSE "${WORK}")
file(MAKE_DIRECTORY "${WORK}")
file(COPY "${SRC}/" DESTINATION "${WORK}/a")

execute_process(COMMAND "${GIT}" init -q WORKING_DIRECTORY "${WORK}/a"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git init failed: ${err}")
endif()
git_commit_all("${WORK}/a" "initial commit")

reconfigure("${WORK}/a" "${WORK}/a-build")
set(CDB_A "${WORK}/a-build/compile_commands.json")
if(NOT EXISTS "${CDB_A}")
    message(FATAL_ERROR "compile_commands.json wasn't produced at ${CDB_A}")
endif()

kg_run("kg index A" "${WORK}" 0
    "${KG}" index -c "${CDB_A}" -o "${WORK}/a.srctrldb" --repo "${WORK}/a" --force)

# --file src/chain.cpp (not --all): chain.cpp's functions all have EXTERNAL
# linkage (level0..level4, runChain, factorial, ping -- no `static`, no
# anonymous namespace). This deliberately sidesteps a separate, documented
# limitation (docs/KNOWN_LIMITATIONS.md's Notes section): a TU-LOCAL
# symbol's serialized_name embeds an absolute-path-derived disambiguator
# hash (see NameBuilder.cpp's disambiguatorFor()), which is NOT stable
# across two worktrees checked out at different filesystem paths -- so a
# note on a `static` function or one inside an anonymous namespace would
# NOT reliably carry over via serialized_name alone the way a normal
# (externally-linked) function's note does. This test proves the property
# that actually matters in practice: ordinary, note-worthy API functions.
message(STATUS "notes_cross_worktree: A -- scaffold chain.cpp + fill + fmt --check + sync")
kg_run("kg notes scaffold --file src/chain.cpp (A)" "${WORK}" 0
    "${KG}" notes scaffold -d "${WORK}/a.srctrldb" --notes-dir "${WORK}/a/notes" --file "src/chain.cpp")

if(DEFINED SCRIPTS AND NOT SCRIPTS STREQUAL "")
    kg_run("kg-notes-fake --fill-existing (A)" "${WORK}" 0
        python3 "${SCRIPTS}/kg-notes-fake.py" --fill-existing "${WORK}/a/notes" --seed 5 --fill-ratio 1.0)
endif()

kg_run("kg notes fmt --check (A)" "${WORK}" 0
    "${KG}" notes fmt --notes-dir "${WORK}/a/notes" --check)

kg_run("kg notes sync (A)" "${WORK}" 0
    "${KG}" notes sync -d "${WORK}/a.srctrldb" --notes-dir "${WORK}/a/notes")

git_commit_all("${WORK}/a" "notes: scaffold + fill for every symbol")

# ------------------------------------------------------------------------
# Step 2: worktree B -- an independent `git clone` of A, to a DIFFERENT path
# ------------------------------------------------------------------------

message(STATUS "notes_cross_worktree: cloning A -> ${WORK}/b")
execute_process(COMMAND "${GIT}" clone -q "${WORK}/a" "${WORK}/b"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git clone A -> B failed: ${err}")
endif()

# B's own, entirely independent index: different build dir (different
# compile_commands.json absolute paths), different output database (fresh
# node/edge ids -- `kg index` never reuses A's ids).
reconfigure("${WORK}/b" "${WORK}/b-build")
set(CDB_B "${WORK}/b-build/compile_commands.json")
if(NOT EXISTS "${CDB_B}")
    message(FATAL_ERROR "compile_commands.json wasn't produced at ${CDB_B}")
endif()

kg_run("kg index B" "${WORK}" 0
    "${KG}" index -c "${CDB_B}" -o "${WORK}/b.srctrldb" --repo "${WORK}/b" --force)

message(STATUS "notes_cross_worktree: B -- sync (notes/ arrived via git clone, never re-scaffolded)")
kg_run("kg notes sync (B)" "${WORK}" 0
    "${KG}" notes sync -d "${WORK}/b.srctrldb" --notes-dir "${WORK}/b/notes")

# ------------------------------------------------------------------------
# Step 3: A and B have completely different node/edge ids and absolute file
# paths -- a plain `kg dbdiff` must NOT be empty -- but `--only note` must
# be, since the notes cache is keyed by serialized_name alone.
# ------------------------------------------------------------------------

message(STATUS "notes_cross_worktree: dbdiff A vs B (plain) must differ (different worktree paths)")
execute_process(
    COMMAND "${KG}" dbdiff "${WORK}/a.srctrldb" "${WORK}/b.srctrldb"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(rc EQUAL 0)
    message(FATAL_ERROR "notes_cross_worktree: expected plain `kg dbdiff A B` to differ (different worktree paths), got exit 0:\n${out}")
endif()

message(STATUS "notes_cross_worktree: dbdiff A vs B --only note must be EMPTY (notes travel with git, independent of the DB)")
execute_process(
    COMMAND "${KG}" dbdiff "${WORK}/a.srctrldb" "${WORK}/b.srctrldb" --only note --json
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "notes_cross_worktree: `kg dbdiff A B --only note` expected exit 0 (equal), got ${rc}:\n${out}\n${err}")
endif()
json_field(v "${out}" equal)
if(NOT v)
    message(FATAL_ERROR "dbdiff --only note: expected equal, got '${v}'\n${out}")
endif()

# --only note_problem must also be empty -- identical markdown, identical
# matching decisions.
execute_process(
    COMMAND "${KG}" dbdiff "${WORK}/a.srctrldb" "${WORK}/b.srctrldb" --only note_problem --json
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "notes_cross_worktree: `kg dbdiff A B --only note_problem` expected exit 0 (equal), got ${rc}:\n${out}\n${err}")
endif()

# ------------------------------------------------------------------------
# Step 4: `kg query context 'level2' --json` in A and B must carry the SAME
# `notes` objects for the same qualified_names.
# ------------------------------------------------------------------------

message(STATUS "notes_cross_worktree: comparing `kg query context level2 --json` notes across A and B")
kg_run("kg query context level2 (A)" "${WORK}" 0
    "${KG}" query context level2 -d "${WORK}/a.srctrldb" --json)
set(CTX_A "${KG_OUT}")
kg_run("kg query context level2 (B)" "${WORK}" 0
    "${KG}" query context level2 -d "${WORK}/b.srctrldb" --json)
set(CTX_B "${KG_OUT}")

json_field(NODES_LEN_A "${CTX_A}" nodes)
string(JSON NODES_LEN_A LENGTH "${CTX_A}" nodes)
string(JSON NODES_LEN_B LENGTH "${CTX_B}" nodes)
assert_eq("context level2: same node count in A and B" "${NODES_LEN_A}" "${NODES_LEN_B}")
if(NODES_LEN_A LESS 1)
    message(FATAL_ERROR "notes_cross_worktree: `kg query context level2` returned 0 nodes")
endif()

math(EXPR LAST_A "${NODES_LEN_A} - 1")
set(COMPARED 0)
foreach(i RANGE ${LAST_A})
    json_field(qnameA "${CTX_A}" nodes ${i} qualified_name)
    json_field(qnameB "${CTX_B}" nodes ${i} qualified_name)
    assert_eq("context level2: node ${i} qualified_name order matches (both sorted by kind+serialized_name)" "${qnameA}" "${qnameB}")

    json_field(notesA "${CTX_A}" nodes ${i} notes)
    json_field(notesB "${CTX_B}" nodes ${i} notes)
    assert_eq("context level2: notes object for '${qnameA}' matches between A and B" "${notesA}" "${notesB}")

    if(NOT notesA STREQUAL "null")
        json_field(filledA "${CTX_A}" nodes ${i} notes filled)
        if(NOT filledA)
            message(FATAL_ERROR "context level2: '${qnameA}' note expected filled (kg-notes-fake --fill-ratio 1.0), got '${filledA}'")
        endif()
        math(EXPR COMPARED "${COMPARED} + 1")
    endif()
endforeach()
if(COMPARED LESS 1)
    message(FATAL_ERROR "notes_cross_worktree: expected at least one filled note among level2's context nodes, compared 0")
endif()

message(STATUS "notes_cross_worktree: ALL STEPS PASSED (compared ${COMPARED} filled note(s) across A/B)")
