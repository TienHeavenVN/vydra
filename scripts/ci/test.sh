#!/usr/bin/env bash
# Run the workspace test suite one package at a time under a hard per-package timeout.
#
# Running the whole workspace in one `cargo test` invocation has twice ended in the runner's
# SIGTERM (exit 143) with no diagnostics: a single hanging test takes the entire job with it and
# nothing is annotated. Per-package execution converts a hang into a timeout (exit 124) that names
# the package responsible, and the aggregated log is annotated afterwards.
set -uo pipefail

log="${1:-test.log}"
per_package_timeout="${TEST_TIMEOUT_SECONDS:-180}"

: >"$log"
failed=0
timed_out=""

packages="$(cargo metadata --no-deps --format-version 1 2>>"$log" \
  | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))')"

for pkg in $packages; do
  echo "::group::cargo test -p $pkg"
  timeout --kill-after=10 "$per_package_timeout" \
    cargo test -p "$pkg" --all-features --message-format=short 2>&1 | tee -a "$log"
  code="${PIPESTATUS[0]}"
  echo "::endgroup::"
  if [ "$code" -eq 124 ] || [ "$code" -eq 137 ]; then
    timed_out="$timed_out $pkg"
    echo "RESULT $pkg exit=$code (timed out after ${per_package_timeout}s)" | tee -a "$log"
    failed=1
  elif [ "$code" -ne 0 ]; then
    echo "RESULT $pkg exit=$code" | tee -a "$log"
    failed=1
  else
    echo "RESULT $pkg exit=0" | tee -a "$log"
  fi
done

if [ -n "$timed_out" ]; then
  echo "timed out:$timed_out" | tee -a "$log"
fi

bash scripts/ci/report.sh "$log" || true
exit "$failed"
