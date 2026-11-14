#!/usr/bin/env bash
# Publish compiler/test diagnostics from a cargo log as check annotations.
#
# `cargo` is run with `--message-format=short`, so diagnostics do not start at column 0 with
# "error": they look like `crates/x/src/lib.rs:10:5: error[E0432]: ...`. Matching only `^error`
# silently produced empty reports, which is why the failing jobs used to show no annotations.
set -uo pipefail

log="${1:?usage: report.sh <cargo-log>}"

if [ ! -f "$log" ]; then
  echo "report.sh: $log not found"
  exit 0
fi

tmp="$(mktemp)"
published=0
note() {
  if [ -s "$tmp" ]; then
    published=1
    bash scripts/ci/annotate.sh "$tmp" -1 "$1" || true
  fi
}

# Compiler diagnostics (short format) plus manifest-level lint findings.
grep -E ': (error|warning)(\[|:)|^(error|warning): ' "$log" | head -80 >"$tmp" || true
note 80

# Test panics with the surrounding assertion text.
grep -B3 -A8 'panicked at' "$log" | head -80 >"$tmp" || true
note 60

# Fallback: context around a target that failed to compile.
grep -B14 -A3 'could not compile' "$log" | head -80 >"$tmp" || true
note 60

# Last resort: an unparsed failure is still better than a silent red job.
if [ "$published" -eq 0 ]; then
  tail -40 "$log" >"$tmp" || true
  bash scripts/ci/annotate.sh "$tmp" -1 40 || true
fi

rm -f "$tmp"
