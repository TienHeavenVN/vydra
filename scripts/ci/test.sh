#!/usr/bin/env bash
# Run the workspace test suite one package at a time under a hard per-package timeout.
#
# Why this exists: a single hanging test used to take the whole job down as exit 143 with no
# diagnostics at all (the runner sends SIGTERM, the shell dies, nothing is annotated). Two rules
# make a hang survivable:
#   1. this script ignores SIGTERM, so a group-wide kill cannot take the driver with it;
#   2. each package runs in a subshell where SIGTERM is restored, so `timeout` can end the test
#      run and the loop still records a verdict.
# A hang therefore shows up as `RESULT <package> exit=143 (timed out)` in the annotations.
set -uo pipefail

log="${1:-test.log}"
per_package_timeout="${TEST_TIMEOUT_SECONDS:-300}"

trap '' TERM INT

: >"$log"
failed=0
timed_out=""

packages="$(cargo metadata --no-deps --format-version 1 2>>"$log" \
  | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))')"

for pkg in $packages; do
  echo "::group::cargo test -p $pkg"
  printf '\n===== %s =====\n' "$pkg" >>"$log"
  (
    trap - TERM INT
    exec timeout --kill-after=10 "$per_package_timeout" \
      cargo test -p "$pkg" --all-features --message-format=short
  ) >>"$log" 2>&1
  code=$?
  echo "::endgroup::"
  case "$code" in
    0) echo "RESULT $pkg exit=0" ;;
    124 | 137 | 143)
      timed_out="$timed_out $pkg"
      failed=1
      echo "RESULT $pkg exit=$code (timed out after ${per_package_timeout}s)"
      ;;
    *)
      failed=1
      echo "RESULT $pkg exit=$code"
      ;;
  esac | tee -a "$log"
done

if [ -n "$timed_out" ]; then
  echo "timed out:$timed_out" | tee -a "$log"
fi

verdicts="${log%.log}.verdicts.log"
grep -E '^(RESULT |timed out:)' "$log" >"$verdicts" || true
bash scripts/ci/annotate.sh "$verdicts" -1 40 || true
bash scripts/ci/report.sh "$log" || true
exit "$failed"
