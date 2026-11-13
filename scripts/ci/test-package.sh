#!/usr/bin/env bash
# Test one package, and if it hangs, identify the test that hangs.
#
# A hanging test is the one failure mode that publishes nothing: cargo never returns, the job is
# killed by the runner and the only visible symptom is an opaque cancellation. So the package run
# is capped, this script ignores SIGTERM (the cap signals the whole process group), and on a
# timeout every test is re-run on its own with a short cap. The offenders are printed as
# `SUSPECT <package> <test> exit=<n>` and annotated with the rest of the log.
set -uo pipefail

pkg="${1:?usage: test-package.sh <package> [log]}"
log="${2:-test.log}"
package_timeout="${TEST_TIMEOUT_SECONDS:-240}"
test_timeout="${TEST_PROBE_SECONDS:-20}"

trap '' TERM

: >"$log"
printf '===== %s =====\n' "$pkg" >>"$log"
timeout --kill-after=10 "$package_timeout" \
  cargo test --package "$pkg" --all-features --message-format=short -- --test-threads=1 \
  >>"$log" 2>&1
code=$?

case "$code" in
  124 | 137 | 143)
    echo "===== $pkg hit the ${package_timeout}s cap; probing tests individually =====" >>"$log"
    names="$(cargo test --package "$pkg" --all-features -- --list --format terse 2>>"$log" \
      | sed -n 's/^\(.*\): test$/\1/p')"
    for name in $names; do
      timeout --kill-after=5 "$test_timeout" \
        cargo test --package "$pkg" --all-features -- --exact "$name" --test-threads=1 \
        >>"$log" 2>&1
      rc=$?
      if [ "$rc" -ne 0 ]; then
        echo "SUSPECT $pkg $name exit=$rc" | tee -a "$log"
      fi
    done
    ;;
esac

bash scripts/ci/report.sh "$log" || true
exit "$code"
