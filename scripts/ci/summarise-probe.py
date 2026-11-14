#!/usr/bin/env python3
"""Turn a Phase 4.3 probe summary into GitHub annotations.

The probe job publishes its capture as a commit; this makes the *structured facts* visible on the
check run even when publishing fails, which is how the last publish failure was diagnosed.
"""

from __future__ import annotations

import json
import pathlib
import sys


def notice(title: str, payload: object) -> None:
    text = json.dumps(payload, ensure_ascii=False)
    print(f"::notice title={title}::{text[:900]}")


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: summarise-probe.py <probe-out/summary.json>", file=sys.stderr)
        return 2
    path = pathlib.Path(sys.argv[1])
    if not path.is_file():
        print(f"no summary at {path}", file=sys.stderr)
        return 0
    summary = json.loads(path.read_text(encoding="utf-8"))
    for key, value in summary.get("probe", {}).items():
        notice(f"probe.{key}", value)
    for record in summary.get("records", []):
        notice(f"probe.{record.get('label', 'record')}", record)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
