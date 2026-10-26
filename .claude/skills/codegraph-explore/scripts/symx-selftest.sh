#!/bin/bash
# Regression checks for symx against a known-good index (originally the "sirius" repo).
# Each case = facts verified by hand against that index; they only pass on a repo whose
# index contains those exact symbols. Binaries symhow/symwhere are found on $PATH.
#
# Usage: symx-selftest.sh [REPO]        (exit 0 = all pass)
#   REPO defaults to $SYMX_SELFTEST_REPO, else the current directory.
# symx locates the DB via $SYMX_DBS (default ~/explorer); set it if your indexes live elsewhere.
# Re-run after editing symx or rebuilding a DB. Adapt the checks below to your own index as needed.
X="$(dirname "$(readlink -f "$0")")/symx"
REPO="${1:-${SYMX_SELFTEST_REPO:-$PWD}}"
pass=0; fail=0
check() {  # check NAME "cmd args" "expected substring" [...more substrings]
  local name=$1 cmd=$2; shift 2
  local out; out=$(cd "$REPO" && eval "$X $cmd" 2>&1)
  for want in "$@"; do
    if ! grep -qF -- "$want" <<<"$out"; then
      echo "FAIL $name: missing '$want'"; echo "$out" | head -8 | sed 's/^/     /'; fail=$((fail+1)); return
    fi
  done
  echo "ok   $name"; pass=$((pass+1))
}

check status-sirius     "status" "sirius.srctrldb" "symwhere" "TUs by status" "unique errors"
check cuda-launch-edge  "at src/op/partition/crc32_partition_hash.cu:324" "crc32_partition_hash::compute" "fold_all"
check tests-catch2      "tests fold_all" "14 test function(s)" "via compute"
check tests-virtual     "tests 'sirius::op::sirius_physical_union::get_name'" "test function(s)" "via base virtual"
check overriders-sir    "overriders 'sirius::op::sirius_physical_operator::get_name'" "sirius_physical_concat::get_name"
check includers-sir     "includers src/op/sirius_physical_operator.hpp" "TUs rebuild on change"
check ambiguous-exit2   "tree 'sirius::op::sirius_physical_operator::get_name' --callers; echo exit=\$?" "exit="

echo "-- $pass passed, $fail failed"
[ $fail -eq 0 ]
