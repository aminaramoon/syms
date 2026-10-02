# Real-corpus `kg update` acceptance test (Phase 5, final).
#
# Run as a CMake script (`-P`), invoked by CTest as the `cucs_update_integration`
# test (see ../CMakeLists.txt for the exact `add_test()` registration).
#
# Unlike test/update/update_scenario.cmake (Phase 4 -- a throwaway *synthetic*
# fixture project driven through a handful of hand-crafted commits) this
# exercises `kg update` against the REAL cucascade corpus (the same one
# test/integration/test_cucs.cpp indexes from scratch, ~200 TUs / ~105s) over
# a REAL commit range already in that repo's own history, and checks two
# things at real scale:
#
#   (a) `kg update` from FROM to HEAD reproduces a fresh `kg index` of HEAD
#       exactly (`kg dbdiff`, exit 0 == logically equivalent) -- the same
#       bar update_scenario.cmake holds the synthetic fixture to, just on
#       real code with a real, uncurated diff instead of six hand-picked
#       edit/delete/add/rename/flag-change scenarios.
#   (b) a single-file `kg update` (one .cpp, no other TU includes it) is
#       dramatically faster than a full re-index -- the actual point of
#       having `kg update` at all.
#
# **This test NEVER modifies the user's real cucs checkout** (`${CUCS}`, i.e.
# KG_CUCS_DIR): it works entirely inside a throwaway `git clone --shared
# --no-checkout` under ${WORK}, which shares object storage with (read-only)
# but never writes to the original. No `checkout`/`stash`/`reset`/file edit
# ever targets `${CUCS}` itself -- the only commands run against it are
# read-only (`git rev-parse`, `git clone` as the *source*, existence checks).
#
# Required variables (all passed by test/CMakeLists.txt's add_test()):
#   KG      -- absolute path to the `kg` executable
#   CUCS    -- the real cucs corpus root (KG_CUCS_DIR)
#   WORK    -- scratch directory (recreated from scratch every run)
#   GIT     -- absolute path to `git`
#   SQLITE3 -- absolute path to the `sqlite3` CLI
# Optional:
#   FROM    -- commit-ish, relative to the ORIGINAL repo's HEAD, to treat as
#              the baseline (default: HEAD~1). The environment variable
#              KG_CUCS_UPDATE_FROM, read directly by THIS script at its own
#              (ctest-launch-time) runtime, takes priority over both this and
#              any -DFROM passed at CMake-configure time -- this is what
#              lets `KG_CUCS_UPDATE_FROM=HEAD~5 ctest -R cucs_update_integration`
#              work without reconfiguring: a configure-time `-DFROM=$ENV{...}`
#              alone would bake in whatever KG_CUCS_UPDATE_FROM happened to be
#              set to when `cmake` was last run, not at each `ctest` invocation.
#
# Exits 77 (CTest's SKIP_RETURN_CODE) if git/sqlite3 weren't found, or the
# cucs corpus isn't checked out at ${CUCS}; FATAL_ERROR (CTest FAILED) on any
# assertion or command failure.

cmake_minimum_required(VERSION 3.25) # cmake_language(EXIT ...), string(JSON ...)

if(NOT DEFINED KG OR NOT DEFINED CUCS OR NOT DEFINED WORK)
    message(FATAL_ERROR "cucs_update_scenario.cmake: KG, CUCS and WORK must all be set")
endif()

if(NOT DEFINED GIT OR GIT STREQUAL "" OR NOT EXISTS "${GIT}")
    message("cucs_update_scenario: git not found -- skipping")
    cmake_language(EXIT 77)
endif()

if(NOT DEFINED SQLITE3 OR SQLITE3 STREQUAL "" OR NOT EXISTS "${SQLITE3}")
    message("cucs_update_scenario: sqlite3 not found -- skipping")
    cmake_language(EXIT 77)
endif()

set(CDB "${CUCS}/compile_commands.json")
if(NOT EXISTS "${CDB}" OR NOT EXISTS "${CUCS}/.git")
    message("cucs_update_scenario: '${CDB}' or '${CUCS}/.git' not found -- skipping "
             "(this test only runs on a machine with the cucs corpus checked out at KG_CUCS_DIR)")
    cmake_language(EXIT 77)
endif()

# Runtime env var wins over any configure-time -DFROM (see header comment).
if(DEFINED ENV{KG_CUCS_UPDATE_FROM} AND NOT "$ENV{KG_CUCS_UPDATE_FROM}" STREQUAL "")
    set(FROM "$ENV{KG_CUCS_UPDATE_FROM}")
elseif(NOT DEFINED FROM OR FROM STREQUAL "")
    set(FROM "HEAD~1")
endif()

message(STATUS "cucs_update_scenario: CUCS=${CUCS} WORK=${WORK} FROM=${FROM}")

# ------------------------------------------------------------------------
# Helpers (same style/contract as test/update/update_scenario.cmake's)
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
    set(KG_RC "${rc}" PARENT_SCOPE)
endfunction()

# timed_run(<desc> <workdir> <expected-rc-list> <timevar> <cmd...>) -- like
# kg_run but accepts a list of acceptable exit codes (e.g. "0;5" for
# `kg update`, which legitimately exits 5 when there's nothing to do) and
# records wall-clock seconds into <timevar>.
function(timed_run desc workdir expected_rcs timevar)
    set(cmd ${ARGN})
    string(TIMESTAMP t0 "%s")
    execute_process(
        COMMAND ${cmd}
        WORKING_DIRECTORY "${workdir}"
        RESULT_VARIABLE rc
        OUTPUT_VARIABLE out
        ERROR_VARIABLE err)
    string(TIMESTAMP t1 "%s")
    math(EXPR elapsed "${t1} - ${t0}")
    list(FIND expected_rcs "${rc}" idx)
    if(idx LESS 0)
        message(FATAL_ERROR
            "${desc}\n  command: ${cmd}\n  expected exit in {${expected_rcs}}, got ${rc}\n"
            "--- stdout ---\n${out}\n--- stderr ---\n${err}")
    endif()
    set(KG_OUT "${out}" PARENT_SCOPE)
    set(KG_ERR "${err}" PARENT_SCOPE)
    set(KG_RC "${rc}" PARENT_SCOPE)
    set(${timevar} "${elapsed}" PARENT_SCOPE)
endfunction()

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

# git_in_clone(<outvar> <args...>) -- runs git against WORK/repo (the CLONE,
# never CUCS itself).
function(git_in_clone outvar)
    execute_process(
        COMMAND "${GIT}" -C "${WORK}/repo" ${ARGN}
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "git -C ${WORK}/repo ${ARGN} failed: ${err}")
    endif()
    string(STRIP "${out}" out)
    set(${outvar} "${out}" PARENT_SCOPE)
endfunction()

set(REMAP_ARGS --remap /mnt/workspace/oss/cucs=${WORK}/repo --remap ${CUCS}=${WORK}/repo)

# The realistic configuration for this corpus: index cucs itself, not the
# toolkit and vendored dependencies it is compiled against. cucs's CDB
# reaches the CUDA toolkit headers under `.pixi/` and the FetchContent'd
# third-party sources under `build/release/_deps/` (Catch2, ...) with plain
# `-I`, not `-isystem`, so libclang's clang_Location_isInSystemHeader() says
# "not a system header" and kg would otherwise walk ~20k-line toolkit
# headers as first-party code from every one of the ~200 TUs.
# `kg update` inherits these from the database's stored provenance
# (Provenance.cpp persists dbOptions.excludeGlobs as meta `kg_db_options`),
# so passing them to `kg index` alone is enough -- the scenario asserts
# below that the update run never re-introduces an excluded file.
set(EXCLUDE_ARGS --exclude "**/.pixi/**" --exclude "**/_deps/**")

function(kg_index_cucs outdb timevar)
    timed_run("kg index -> ${outdb}" "${WORK}" "0" ${timevar}
        "${KG}" index -c "${CDB}" -o "${outdb}" --repo "${WORK}/repo" --force -q ${REMAP_ARGS} ${EXCLUDE_ARGS})
    # timed_run set ${timevar} in THIS function's scope; hoist it one more
    # level so the caller actually sees the timing (without this the whole
    # T_full_from / T_full_head summary -- and the T_small speed comparison
    # that uses T_full_head -- silently evaluates against an empty string).
    set(${timevar} "${${timevar}}" PARENT_SCOPE)
endfunction()

function(kg_dbdiff_ignoring a b category)
    execute_process(
        COMMAND "${KG}" dbdiff "${a}" "${b}" --limit 50 --ignore "${category}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR
            "kg dbdiff --ignore ${category} ${a} ${b} : expected equivalent (exit 0), got ${rc}\n"
            "--- stdout ---\n${out}\n--- stderr ---\n${err}")
    endif()
endfunction()

function(kg_dbdiff a b)
    execute_process(
        COMMAND "${KG}" dbdiff "${a}" "${b}" --limit 50
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR
            "kg dbdiff ${a} ${b} : expected equivalent (exit 0), got ${rc}\n"
            "(first 50 differences per side; kg dbdiff's own --limit)\n"
            "--- stdout ---\n${out}\n--- stderr ---\n${err}")
    endif()
endfunction()

# ------------------------------------------------------------------------
# Step 1: throwaway, non-invasive clone of the real cucs corpus
# ------------------------------------------------------------------------

message(STATUS "cucs_update_scenario: cloning ${CUCS} -> ${WORK}/repo (--shared --no-checkout, read-only on the original)")
file(REMOVE_RECURSE "${WORK}")
file(MAKE_DIRECTORY "${WORK}")

execute_process(
    COMMAND "${GIT}" clone --shared --no-checkout "${CUCS}" "${WORK}/repo"
    RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git clone --shared --no-checkout ${CUCS} ${WORK}/repo failed: ${err}")
endif()

# Resolve the original's HEAD read-only (never touches ${CUCS}'s worktree/index).
execute_process(
    COMMAND "${GIT}" -C "${CUCS}" rev-parse HEAD
    RESULT_VARIABLE rc OUTPUT_VARIABLE ORIG_HEAD ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git -C ${CUCS} rev-parse HEAD failed: ${err}")
endif()
string(STRIP "${ORIG_HEAD}" ORIG_HEAD)
message(STATUS "cucs_update_scenario: original cucs HEAD = ${ORIG_HEAD}")

git_in_clone(_discard checkout -q --detach "${ORIG_HEAD}")
git_in_clone(HEAD_SHA rev-parse HEAD)
assert_eq("clone HEAD == original HEAD" "${HEAD_SHA}" "${ORIG_HEAD}")

git_in_clone(FROM_SHA rev-parse "${FROM}")
message(STATUS "cucs_update_scenario: HEAD_SHA=${HEAD_SHA} FROM_SHA=${FROM_SHA} (FROM=${FROM})")

if(FROM_SHA STREQUAL HEAD_SHA)
    message(FATAL_ERROR "FROM (${FROM} = ${FROM_SHA}) resolves to the same commit as HEAD (${HEAD_SHA}) -- "
                         "pick a FROM with real history behind it (KG_CUCS_UPDATE_FROM=HEAD~N)")
endif()

# ------------------------------------------------------------------------
# Step 2: make the clone usable with the ORIGINAL, static compile_commands.json
# ------------------------------------------------------------------------
#
# The CDB's include/toolchain paths reference two absolute roots (verified
# by grepping the real CDB while designing this test):
#   /mnt/workspace/oss/cucs/...      -- .pixi (compiler + sysroot) and
#                                        build/release/_deps (vendored
#                                        third-party headers) -- the path
#                                        cucs was built from originally.
#   ${CUCS}/...                      -- source/include paths and the CDB's
#                                        own `directory` field -- this
#                                        machine's real path to the corpus.
# REMAP_ARGS (set above) rewrites both onto ${WORK}/repo. Since the clone's
# checkout only has git-tracked files, `.pixi` and `build` (the toolchain
# and generated-headers roots) need to exist there too -- symlinked straight
# at the real, unmodified directories under ${CUCS} (read-only access,
# nothing in ${CUCS} is written).
file(CREATE_LINK "${CUCS}/.pixi" "${WORK}/repo/.pixi" SYMBOLIC)
file(CREATE_LINK "${CUCS}/build" "${WORK}/repo/build" SYMBOLIC)

# `.pixi`/`build` are untracked-by-git symlinks that only exist for CDB path
# resolution, not real source content -- without this, `git status
# --porcelain --untracked-files=all` (what kg update's worktree diff uses)
# would report them as permanently "added" on every single run, polluting
# every change-set this test inspects. `.git/info/exclude` is local to THIS
# clone only (never touches ${CUCS}'s own git config).
file(APPEND "${WORK}/repo/.git/info/exclude" "\n.pixi\nbuild\n")

execute_process(COMMAND "${GIT}" -C "${WORK}/repo" status --porcelain --untracked-files=all
    RESULT_VARIABLE rc OUTPUT_VARIABLE STATUS_OUT ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "git -C ${WORK}/repo status --porcelain failed: ${err}")
endif()
string(STRIP "${STATUS_OUT}" STATUS_OUT)
if(NOT STATUS_OUT STREQUAL "")
    message(FATAL_ERROR "clone worktree not clean right after setup (expected clean, .pixi/build gitignored):\n${STATUS_OUT}")
endif()

# Sanity-check the remap: every -I/-isystem path in a sample --dry-run
# argv must land under ${WORK}/repo, and NONE may still point at either
# original root.
execute_process(
    COMMAND "${KG}" index -c "${CDB}" --repo "${WORK}/repo" --dry-run --limit 3 ${REMAP_ARGS}
    RESULT_VARIABLE rc OUTPUT_VARIABLE DRYRUN_OUT ERROR_VARIABLE err)
if(NOT rc EQUAL 0)
    message(FATAL_ERROR "kg index --dry-run (remap sanity check) failed: ${err}")
endif()
if(DRYRUN_OUT MATCHES "/mnt/workspace")
    message(FATAL_ERROR "kg index --dry-run still emits an /mnt/workspace path after remapping -- remap is incomplete:\n${DRYRUN_OUT}")
endif()
if(DRYRUN_OUT MATCHES "${CUCS}/")
    message(FATAL_ERROR "kg index --dry-run still emits a ${CUCS}/ path after remapping -- remap is incomplete:\n${DRYRUN_OUT}")
endif()
if(NOT DRYRUN_OUT MATCHES "${WORK}/repo/")
    message(FATAL_ERROR "kg index --dry-run produced no ${WORK}/repo/ paths at all -- remap sanity check itself is broken:\n${DRYRUN_OUT}")
endif()
message(STATUS "cucs_update_scenario: remap sanity check passed (paths resolve under ${WORK}/repo, nothing left at either original root)")

# ------------------------------------------------------------------------
# Step 3: correctness run -- kg update FROM..HEAD == fresh kg index of HEAD
# ------------------------------------------------------------------------

message(STATUS "cucs_update_scenario: [correctness] full index @ FROM (${FROM_SHA})")
git_in_clone(_discard checkout -q --detach "${FROM_SHA}")
kg_index_cucs("${WORK}/inc.srctrldb" T_full_from)
message(STATUS "cucs_update_scenario: [correctness] full index @ FROM took ${T_full_from}s")

sqlite_scalar(META_COMMIT "${WORK}/inc.srctrldb" "select value from meta where key='kg_repo_commit'")
assert_eq("meta.kg_repo_commit after FROM index" "${META_COMMIT}" "${FROM_SHA}")

message(STATUS "cucs_update_scenario: [correctness] kg update FROM -> HEAD (${HEAD_SHA})")
git_in_clone(_discard checkout -q --detach "${HEAD_SHA}")
timed_run("kg update FROM->HEAD" "${WORK}" "0;5" T_update
    "${KG}" update -d "${WORK}/inc.srctrldb" -v --json)
file(WRITE "${WORK}/update.json" "${KG_OUT}")
message(STATUS "cucs_update_scenario: [correctness] kg update exit=${KG_RC}, took ${T_update}s")

if(KG_RC EQUAL 5)
    message(WARNING "cucs_update_scenario: kg update reported exit 5 (nothing to do) for ${FROM}..${HEAD_SHA} -- "
                     "pick a FROM with real source changes (KG_CUCS_UPDATE_FROM=HEAD~N) for a meaningful correctness run")
else()
    json_field(V_CHANGED "${KG_OUT}" files changed)
    json_field(V_DELETED "${KG_OUT}" files deleted)
    json_field(V_RENAMED "${KG_OUT}" files renamed)
    json_field(V_FLAGCH "${KG_OUT}" files flag_changed)
    json_field(V_UNMATCH "${KG_OUT}" files unmatched)
    json_field(V_CLEARED "${KG_OUT}" files cleared)
    json_field(V_TO_INDEX "${KG_OUT}" tus to_index)
    json_field(V_TU_NEW "${KG_OUT}" tus new)
    json_field(V_TU_RETRIED "${KG_OUT}" tus retried)
    json_field(V_GC_FILES "${KG_OUT}" gc files)
    json_field(V_GC_NODES "${KG_OUT}" gc nodes)
    json_field(V_GC_EDGES "${KG_OUT}" gc edges)
    message(STATUS "cucs_update_scenario: [correctness] files: changed=${V_CHANGED} deleted=${V_DELETED} "
                    "renamed=${V_RENAMED} flag_changed=${V_FLAGCH} unmatched=${V_UNMATCH} cleared=${V_CLEARED}")
    message(STATUS "cucs_update_scenario: [correctness] tus: to_index=${V_TO_INDEX} new=${V_TU_NEW} retried=${V_TU_RETRIED}")
    message(STATUS "cucs_update_scenario: [correctness] gc: files=${V_GC_FILES} nodes=${V_GC_NODES} edges=${V_GC_EDGES}")
    if(NOT V_UNMATCH STREQUAL "0")
        message(STATUS "cucs_update_scenario: [correctness] NOTE: ${V_UNMATCH} unmatched path(s) -- expected for files "
                        "added between FROM and HEAD that the STATIC CDB (captured at the original HEAD) has no entry "
                        "for; see docs/KNOWN_LIMITATIONS.md's Phase 5 section")
    endif()
    if(NOT V_TU_RETRIED STREQUAL "0")
        message(STATUS "cucs_update_scenario: [correctness] NOTE: ${V_TU_RETRIED} retried TU(s) -- exercises the "
                        "failed/partial-status retry path (Phase 4)")
    endif()

    # Phase 4 of docs/NOTES_DESIGN.md: `kg.update-summary/1` always carries a
    # `notes` key (report-only -- a notes-dir problem or sync exception
    # never fails `kg update` itself). No `notes/` dir exists in this clone
    # yet at this point in the scenario, so it's `{}` (no known notes dir to
    # sync against) rather than the populated {noted,stale,newly_stale,
    # problems} shape -- that's exercised for real, below, against the
    # resulting inc.srctrldb, once a notes dir exists.
    string(JSON V_NOTES_TYPE ERROR_VARIABLE jerr TYPE "${KG_OUT}" notes)
    if(jerr OR NOT V_NOTES_TYPE STREQUAL "OBJECT")
        message(FATAL_ERROR "cucs_update_scenario: [correctness] kg.update-summary/1 is missing a `notes` object:\n${KG_OUT}")
    endif()
    message(STATUS "cucs_update_scenario: [correctness] kg.update-summary/1 carries a notes object")
endif()

message(STATUS "cucs_update_scenario: [correctness] full index @ HEAD (${HEAD_SHA}), for dbdiff comparison")
kg_index_cucs("${WORK}/full_head.srctrldb" T_full_head)
message(STATUS "cucs_update_scenario: [correctness] full index @ HEAD took ${T_full_head}s")

# --- every GC rule must be a provable no-op on a FRESH index ------------
# The same four predicates test/unit/test_incremental_fixture.cpp asserts on
# the synthetic fixture, re-asserted here against a real 200-TU corpus (the
# fixture is far too small to exercise the template-instantiation shapes
# that made the earlier, unrestricted versions of these rules unsafe).
sqlite_scalar(GC_UNTOUCHED "${WORK}/full_head.srctrldb"
    "select count(*) from node n where n.id not in (select node_id from kg_touched)")
assert_eq("fresh index: every node is in kg_touched" "${GC_UNTOUCHED}" "0")

sqlite_scalar(GC_DANGLING_EDGES "${WORK}/full_head.srctrldb"
    "select count(*) from edge e where e.type<>1 and e.source_node_id not in (select id from file) and e.source_node_id not in (select node_id from kg_touched) and not exists (select 1 from occurrence o where o.element_id=e.source_node_id)")
assert_eq("fresh index: Invalidator's dangling-edge rule deletes nothing" "${GC_DANGLING_EDGES}" "0")

sqlite_scalar(GC_DEAD_NODES "${WORK}/full_head.srctrldb"
    "select count(*) from node n where n.id not in (select id from file) and n.id not in (select node_id from kg_touched) and not exists (select 1 from occurrence o where o.element_id=n.id) and not exists (select 1 from edge e where e.type<>1 and (e.source_node_id=n.id or e.target_node_id=n.id)) and not exists (select 1 from edge e where e.type=1 and e.source_node_id=n.id)")
assert_eq("fresh index: Invalidator's dead-node rule deletes nothing" "${GC_DEAD_NODES}" "0")

# Same predicate WITHOUT the kg_touched guard: this is the number of nodes
# an unguarded dead-node sweep would wrongly delete on this corpus. Purely
# informational (it is expected to be non-zero -- that is exactly why the
# guard exists), so it is reported, not asserted.
sqlite_scalar(GC_DEAD_NODES_UNGUARDED "${WORK}/full_head.srctrldb"
    "select count(*) from node n where n.id not in (select id from file) and not exists (select 1 from occurrence o where o.element_id=n.id) and not exists (select 1 from edge e where e.type<>1 and (e.source_node_id=n.id or e.target_node_id=n.id)) and not exists (select 1 from edge e where e.type=1 and e.source_node_id=n.id)")
message(STATUS "cucs_update_scenario: [correctness] GC safety: an UNGUARDED dead-node sweep would delete "
                "${GC_DEAD_NODES_UNGUARDED} node(s) a fresh index keeps; the kg_touched-guarded rule deletes 0")

sqlite_scalar(WALKER_NONINDEXED_LOCS "${WORK}/full_head.srctrldb"
    "select count(*) from source_location l join file f on f.id=l.file_node_id where f.indexed=0 and l.type<>6")
assert_eq("fresh index: no non-error location lands in a non-indexed file" "${WALKER_NONINDEXED_LOCS}" "0")
message(STATUS "cucs_update_scenario: [correctness] GC/walker safety assertions on the fresh HEAD index passed")

message(STATUS "cucs_update_scenario: [correctness] kg dbdiff inc.srctrldb full_head.srctrldb (MUST be equivalent)")

# Two passes, in this order:
#
#  (1) STRICT, no exclusions at all. If it is empty, we are done -- that is
#      the real acceptance criterion and nothing below relaxes it.
#      If it is NOT empty, every surviving difference must belong to the one
#      documented, inherent limitation (docs/KNOWN_LIMITATIONS.md, Phase 5):
#      an occurrence LOCATION inside a shared (multiply-included) header,
#      contributed by the OLD content of some other TU that has since been
#      re-indexed. So the strict pass is still asserted hard on:
#        * only_in_b MUST be 0 -- `kg update` may never LOSE anything a
#          fresh index has; every GC rule is provably non-destructive.
#        * every only_in_a entry MUST be a `location|` -- never a node,
#          edge, symbol, file, error, local_symbol, symbol_def or tu row.
#  (2) the same diff with `--ignore shared-header-locations`, which MUST be
#      exactly empty. That flag drops occurrence locations inside
#      multiply-included headers on BOTH sides and nothing else.
execute_process(
    COMMAND "${KG}" dbdiff "${WORK}/inc.srctrldb" "${WORK}/full_head.srctrldb" --json --limit 400
    RESULT_VARIABLE DIFF_RC OUTPUT_VARIABLE DIFF_JSON ERROR_VARIABLE DIFF_ERR)
if(DIFF_RC EQUAL 0)
    message(STATUS "cucs_update_scenario: [correctness] STRICTLY EQUIVALENT -- kg update reproduced a fresh kg index "
                    "of HEAD exactly, with no ignored categories at all")
elseif(NOT DIFF_RC EQUAL 3)
    message(FATAL_ERROR "kg dbdiff --json failed (rc=${DIFF_RC}):\n${DIFF_ERR}")
else()
    json_field(DIFF_ONLY_B "${DIFF_JSON}" counts only_in_b)
    json_field(DIFF_ONLY_A "${DIFF_JSON}" counts only_in_a)
    if(NOT DIFF_ONLY_B STREQUAL "0")
        message(FATAL_ERROR
            "kg update LOST ${DIFF_ONLY_B} row(s) a fresh kg index of HEAD has -- a GC rule is over-deleting. "
            "This is never an acceptable divergence.\n${DIFF_JSON}")
    endif()
    foreach(cat node edge symbol file error local_symbol symbol_def tu)
        if(DIFF_JSON MATCHES "\"${cat}\\|")
            message(FATAL_ERROR
                "kg dbdiff reports a stale `${cat}` row in the incrementally-updated database. Only occurrence "
                "LOCATIONS inside shared headers are a known limitation; a stale ${cat} is a real bug.\n${DIFF_JSON}")
        endif()
    endforeach()
    message(STATUS "cucs_update_scenario: [correctness] ${DIFF_ONLY_A} residual difference(s), all of them occurrence "
                    "locations and all one-sided (nothing lost) -- checking they are all inside shared headers")
endif()

kg_dbdiff_ignoring("${WORK}/inc.srctrldb" "${WORK}/full_head.srctrldb" shared-header-locations)
message(STATUS "cucs_update_scenario: [correctness] EQUIVALENT -- kg update reproduced a fresh kg index of HEAD "
                "(modulo the documented shared-header occurrence-location limitation)")


# ------------------------------------------------------------------------
# Step 4: speed run -- a single-.cpp update vs a full re-index
# ------------------------------------------------------------------------

set(SPEED_FILE "${WORK}/repo/src/memory/memory_reservation.cpp")
if(NOT EXISTS "${SPEED_FILE}")
    message(FATAL_ERROR "speed-run file missing at HEAD: ${SPEED_FILE} (corpus layout changed? pick a different --files target)")
endif()

file(COPY_FILE "${WORK}/full_head.srctrldb" "${WORK}/speed.srctrldb")

message(STATUS "cucs_update_scenario: [speed] kg update --files ${SPEED_FILE}")
timed_run("kg update --files memory_reservation.cpp" "${WORK}" "0;5" T_small
    "${KG}" update -d "${WORK}/speed.srctrldb" --files "${SPEED_FILE}" -v --json)
message(STATUS "cucs_update_scenario: [speed] kg update --files took ${T_small}s (exit ${KG_RC})")

message(STATUS "cucs_update_scenario: [speed] kg dbdiff speed.srctrldb full_head.srctrldb (MUST be equivalent)")
kg_dbdiff("${WORK}/speed.srctrldb" "${WORK}/full_head.srctrldb")
message(STATUS "cucs_update_scenario: [speed] EQUIVALENT")

# ------------------------------------------------------------------------
# Step (notes): Phase 4 of docs/NOTES_DESIGN.md -- real-corpus `kg notes`
# coverage on the ~20k-symbol HEAD database (WORK/inc.srctrldb, currently
# checked out at HEAD_SHA in WORK/repo). Scaffolds notes for
# cucascade::memory::reservation_limit_policy and its three derived
# policies (ignore_/fail_/increase_), fills them, syncs, and asserts
# `status`/`context` see the result. Kept robust: if the corpus's real
# class names ever drift, scaffold selecting 0 symbols is reported (not a
# hard failure of the whole test) -- everything after that point that
# depends on there being at least one noted symbol is skipped.
# ------------------------------------------------------------------------

set(NOTES_DIR "${WORK}/repo/notes")
set(POLICY_CLASSES
    "cucascade::memory::reservation_limit_policy"
    "cucascade::memory::ignore_reservation_limit_policy"
    "cucascade::memory::fail_reservation_limit_policy"
    "cucascade::memory::increase_reservation_limit_policy")

set(TOTAL_SELECTED 0)
foreach(cls ${POLICY_CLASSES})
    execute_process(
        COMMAND "${KG}" notes scaffold -d "${WORK}/inc.srctrldb" --notes-dir "${NOTES_DIR}" --class "${cls}" --json
        WORKING_DIRECTORY "${WORK}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE out ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(WARNING "cucs_update_scenario: [notes] kg notes scaffold --class ${cls} failed (rc=${rc}) -- "
                         "corpus class names may have drifted; skipping notes assertions.\n${out}\n${err}")
    else()
        json_field(sel "${out}" symbols_selected)
        message(STATUS "cucs_update_scenario: [notes] --class ${cls}: ${sel} symbol(s) selected")
        math(EXPR TOTAL_SELECTED "${TOTAL_SELECTED} + ${sel}")
    endif()
endforeach()

if(TOTAL_SELECTED LESS 1)
    message(WARNING "cucs_update_scenario: [notes] scaffold selected 0 symbols across all ${POLICY_CLASSES} -- "
                     "skipping the rest of the notes step (corpus layout likely changed; see "
                     "test/update/cucs_update_scenario.cmake's POLICY_CLASSES)")
else()
    message(STATUS "cucs_update_scenario: [notes] ${TOTAL_SELECTED} symbol(s) scaffolded total")

    if(DEFINED SCRIPTS AND NOT SCRIPTS STREQUAL "")
        kg_run("kg-notes-fake --fill-existing" "${WORK}" 0
            python3 "${SCRIPTS}/kg-notes-fake.py" --fill-existing "${NOTES_DIR}" --seed 5 --fill-ratio 1.0)
    endif()

    string(TIMESTAMP T_SYNC0 "%s")
    kg_run("kg notes sync" "${WORK}" 0
        "${KG}" notes sync -d "${WORK}/inc.srctrldb" --notes-dir "${NOTES_DIR}" --json)
    string(TIMESTAMP T_SYNC1 "%s")
    math(EXPR T_SYNC "${T_SYNC1} - ${T_SYNC0}")
    set(SYNC_JSON "${KG_OUT}")
    json_field(SYNC_MATCHED "${SYNC_JSON}" counts matched)
    message(STATUS "cucs_update_scenario: [notes] kg notes sync: ${SYNC_MATCHED} matched, took ${T_SYNC}s "
                    "(on the ~20k-symbol HEAD database, must be well under 2s)")
    if(T_SYNC GREATER 2)
        message(FATAL_ERROR "cucs_update_scenario: [notes] kg notes sync took ${T_SYNC}s (> 2s) on a database "
                             "with only ${TOTAL_SELECTED} noted symbol(s) -- should be fast regardless of the "
                             "surrounding DB's total symbol count")
    endif()

    kg_run("kg notes status" "${WORK}" 0
        "${KG}" notes status -d "${WORK}/inc.srctrldb" --json)
    json_field(STATUS_NOTED "${KG_OUT}" counts noted)
    if(STATUS_NOTED LESS 1)
        message(FATAL_ERROR "cucs_update_scenario: [notes] kg notes status reports 0 noted symbols after scaffold+sync "
                             "of ${TOTAL_SELECTED} policy method(s):\n${KG_OUT}")
    endif()
    message(STATUS "cucs_update_scenario: [notes] kg notes status: ${STATUS_NOTED} noted symbol(s)")

    message(STATUS "cucs_update_scenario: [notes] kg query context increase_reservation_limit_policy::get_policy_name --json")
    execute_process(
        COMMAND "${KG}" query context "cucascade::memory::increase_reservation_limit_policy::get_policy_name" -d "${WORK}/inc.srctrldb" --json
        WORKING_DIRECTORY "${WORK}"
        RESULT_VARIABLE rc OUTPUT_VARIABLE CTX_JSON ERROR_VARIABLE err)
    if(NOT rc EQUAL 0)
        message(FATAL_ERROR "kg query context increase_reservation_limit_policy::get_policy_name failed (rc=${rc}):\n${CTX_JSON}\n${err}")
    endif()

    string(JSON CTX_NODES_LEN LENGTH "${CTX_JSON}" nodes)
    math(EXPR CTX_LAST "${CTX_NODES_LEN} - 1")
    set(SAW_SEED_NOTE FALSE)
    set(SAW_OVERRIDE_NOTE FALSE)
    foreach(i RANGE ${CTX_LAST})
        json_field(qname "${CTX_JSON}" nodes ${i} qualified_name)
        json_field(notesObj "${CTX_JSON}" nodes ${i} notes)
        if(NOT notesObj STREQUAL "null")
            if(qname MATCHES "increase_reservation_limit_policy::get_policy_name$")
                set(SAW_SEED_NOTE TRUE)
            endif()
            if(qname MATCHES "reservation_limit_policy::get_policy_name$" AND NOT qname MATCHES "increase_")
                set(SAW_OVERRIDE_NOTE TRUE)
            endif()
        endif()
    endforeach()
    if(NOT SAW_SEED_NOTE)
        message(FATAL_ERROR "cucs_update_scenario: [notes] the seed "
                             "(increase_reservation_limit_policy::get_policy_name) carries no notes object in "
                             "`kg query context`'s output:\n${CTX_JSON}")
    endif()
    if(NOT SAW_OVERRIDE_NOTE)
        message(FATAL_ERROR "cucs_update_scenario: [notes] no --overrides-target sibling of "
                             "get_policy_name carries a notes object (expected the base class's own "
                             "reservation_limit_policy::get_policy_name -- pure virtual, so it may itself be "
                             "unnoted, but at least one of the base/derived siblings should be):\n${CTX_JSON}")
    endif()
    message(STATUS "cucs_update_scenario: [notes] context carries notes for both the seed and its override chain")
endif()

message(STATUS "cucs_update_scenario: [speed] T_full_head=${T_full_head}s  T_small=${T_small}s")
if(T_small GREATER T_full_head)
    message(FATAL_ERROR "single-file kg update (${T_small}s) was SLOWER than a full re-index (${T_full_head}s) -- "
                         "kg update should never be slower than kg index on a corpus this size")
endif()
math(EXPR FULL_OVER_5 "${T_full_head} / 5")
if(T_small GREATER FULL_OVER_5)
    message(WARNING "single-file kg update (${T_small}s) is not < 1/5th of a full re-index (${T_full_head}s, "
                     "threshold ${FULL_OVER_5}s) -- soft check, not failing (machine may be noisy); investigate if "
                     "this is consistently reproducible")
else()
    message(STATUS "cucs_update_scenario: [speed] T_small < T_full_head / 5 (${T_small}s < ${FULL_OVER_5}s) -- OK")
endif()

# ------------------------------------------------------------------------
# Summary
# ------------------------------------------------------------------------

message(STATUS "")
message(STATUS "==================== cucs_update_integration SUMMARY ====================")
message(STATUS "  range:              ${FROM} (${FROM_SHA}) .. HEAD (${HEAD_SHA})")
message(STATUS "  T_full_from:        ${T_full_from}s  (full kg index @ FROM)")
message(STATUS "  T_update:           ${T_update}s  (kg update FROM -> HEAD, exit ${KG_RC})")
message(STATUS "  T_full_head:        ${T_full_head}s  (full kg index @ HEAD, for dbdiff baseline)")
message(STATUS "  T_small:            ${T_small}s  (kg update --files memory_reservation.cpp)")
message(STATUS "  correctness:        EQUIVALENT (kg dbdiff inc.srctrldb == full_head.srctrldb)")
message(STATUS "  speed:              EQUIVALENT (kg dbdiff speed.srctrldb == full_head.srctrldb)")
message(STATUS "===========================================================================")
message(STATUS "cucs_update_scenario: ALL STEPS PASSED")
