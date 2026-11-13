#!/usr/bin/env bash
# Convert cargo/rustc/rustfmt output into GitHub check-run annotations.
#
# The CI sandbox used for this repository cannot download job logs, so compiler diagnostics are
# surfaced as annotations, which are retrievable through the check-runs API. Ten annotations per
# invocation (GitHub's cap); call repeatedly with an increasing offset for more.
#
# usage: annotate.sh <logfile> [offset] [limit]
set -u

log="${1:?usage: annotate.sh <logfile> [offset] [limit]}"
offset="${2:-0}"
limit="${3:-10}"

python3 - "$log" "$offset" "$limit" <<'PY'
import re
import sys

log, offset, limit = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
try:
    with open(log, errors="replace") as handle:
        lines = handle.read().splitlines()
except FileNotFoundError:
    sys.exit(0)

diag = re.compile(r"^(?P<file>[^\s:][^:]*\.rs):(?P<line>\d+):(?P<col>\d+):\s*(?P<msg>.*)$")
diff = re.compile(r"^Diff in (?P<file>.+?) at line (?P<line>\d+):$")

annotations = []
seen = set()
for raw in lines:
    line = raw.rstrip()
    if not line.strip() or line.lstrip().startswith("|"):
        continue
    match = diag.match(line)
    if match:
        msg = match.group("msg").strip()
        if not msg:
            continue
        key = (match.group("file"), match.group("line"), msg)
        if key in seen:
            continue
        seen.add(key)
        annotations.append(
            "::error file={file},line={line},col={col}::{msg}".format(
                file=match.group("file"),
                line=match.group("line"),
                col=match.group("col"),
                msg=msg[:900],
            )
        )
        continue
    match = diff.match(line)
    if match:
        annotations.append(
            "::error file={file},line={line}::rustfmt would reformat this file".format(
                file=match.group("file"), line=match.group("line")
            )
        )
        continue
    lowered = line.lower()
    if "error" in lowered or "panicked" in lowered or "failed to" in lowered:
        annotations.append("::error title=cargo::" + line[:900])

window = annotations[offset : offset + limit]
for annotation in window:
    print(annotation)
if offset == 0 and not window:
    print("::error title=ci::no diagnostics were extracted; see the job's raw log")
PY
