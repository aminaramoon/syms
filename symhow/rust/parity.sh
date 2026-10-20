#!/usr/bin/env bash
# C++/Rust parity harness for symhow.
#
# Proves the Rust backend reproduces the C++ backend's output on the shared
# synthetic fixture (and any compile_commands.json you point it at):
#
#   1. Builds both backends (C++ via cmake, Rust via cargo).
#   2. Indexes the SAME compile_commands.json with both binaries.
#   3. Logically diffs the two .srctrldb files with parity_compare
#      (same take_snapshot/diff_snapshots core as `symhow dbdiff`).
#   4. Diffs `query --json` output for a fixed list of verbs.
#
# Usage (from the symhow repo root, inside the flox env):
#   ./rust/parity.sh [compile_commands.json]
#
# Exit 0 = parity holds; nonzero = divergence (diff printed to stdout).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="${REPO_ROOT}/rust"
CDB="${1:-${REPO_ROOT}/cpp/build/fixture_build/compile_commands.json}"
WORK="$(mktemp -d /tmp/symhow_parity.XXXXXX)"
trap 'rm -rf "${WORK}"' EXIT

echo "== parity: inputs"
echo "   repo root : ${REPO_ROOT}"
echo "   cdb      : ${CDB}"
test -f "${CDB}" || { echo "error: compile_commands.json not found at ${CDB} (build the C++ tree first: cd cpp && cmake --preset dev && cmake --build --preset dev)"; exit 1; }

echo "== parity: build C++ backend"
# Configure into a private build dir: never mutate the shared `build/` tree
# (its CTest registrations and fixture DBs belong to the C++ test suite).
cmake -S "${REPO_ROOT}/cpp" -B "${WORK}/cpp-build" -G Ninja -DCMAKE_BUILD_TYPE=Debug -DSYMHOW_BUILD_TESTS=OFF >/dev/null
cmake --build "${WORK}/cpp-build" >/dev/null
CPP_BIN="${WORK}/cpp-build/symhow"
test -x "${CPP_BIN}" || { echo "error: C++ binary missing"; exit 1; }

echo "== parity: build Rust backend"
( cd "${RUST_DIR}" && cargo build --release --example parity_compare >/dev/null && cargo build --release --bin symhow >/dev/null )
RUST_BIN="${RUST_DIR}/target/release/symhow"
PARITY="${RUST_DIR}/target/release/examples/parity_compare"
test -x "${RUST_BIN}" || { echo "error: Rust binary missing"; exit 1; }

echo "== parity: index with C++ backend"
"${CPP_BIN}" index -c "${CDB}" -o "${WORK}/cpp.srctrldb" --force -q

echo "== parity: index with Rust backend"
"${RUST_BIN}" index -c "${CDB}" -o "${WORK}/rust.srctrldb" --force -q

echo "== parity: logical database diff"
set +e
"${PARITY}" "${WORK}/cpp.srctrldb" "${WORK}/rust.srctrldb"
DB_STATUS=$?
set -e
if [ "${DB_STATUS}" -eq 0 ]; then
  echo "== parity: databases logically equal"
else
  echo "== parity: DIVERGENCE (exit ${DB_STATUS})"
  exit "${DB_STATUS}"
fi

echo "== parity: query --json diff"
# The envelopes echo the queried db PATH and a wall-clock elapsed_ms; both
# differ by construction between the two runs, so normalize them first.
normalize() {
  sed -e "s|${WORK}/cpp.srctrldb|<DB>|g" -e "s|${WORK}/rust.srctrldb|<DB>|g" -e 's/"elapsed_ms":[0-9]*/"elapsed_ms":0/g' "$1"
}
QUERY_FAILURE=0
for verb in "stats" "search sqr" "references fixture::Square::area" "definition fixture::Square" "callers fixture::Square::area" "callees fixture::Square::area" "file src/chain.cpp" "context level2 fixture::Square"; do
  # Query verbs legitimately exit nonzero (2 = ambiguous, 3 = not found), so
  # disable errexit for the loop body; statuses are compared explicitly.
  set +e
  # shellcheck disable=SC2086
  "${CPP_BIN}" query ${verb} -d "${WORK}/cpp.srctrldb" --json > "${WORK}/cpp.json" 2>/dev/null
  CPP_STATUS=$?
  # shellcheck disable=SC2086
  "${RUST_BIN}" query ${verb} -d "${WORK}/rust.srctrldb" --json > "${WORK}/rust.json" 2>/dev/null
  RUST_STATUS=$?
  set -e
  if [ "${CPP_STATUS}" -ne "${RUST_STATUS}" ]; then
    echo "   verb '${verb}': exit codes differ (cpp=${CPP_STATUS} rust=${RUST_STATUS})"
    QUERY_FAILURE=1
    continue
  fi
  if ! diff -u <(normalize "${WORK}/cpp.json") <(normalize "${WORK}/rust.json") > "${WORK}/verb.diff"; then
    echo "   verb '${verb}': JSON output differs:"
    cat "${WORK}/verb.diff"
    QUERY_FAILURE=1
  else
    echo "   verb '${verb}': identical"
  fi
done

if [ "${QUERY_FAILURE}" -ne 0 ]; then
  echo "== parity: QUERY DIVERGENCE"
  exit 5
fi

echo "== parity: OK (databases and query output identical)"