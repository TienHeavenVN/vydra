#!/usr/bin/env python3
"""Archive a probe output directory as committed evidence.

The probes (`examples/probe_search.rs`, `examples/probe_player.rs`) write redacted JSON into
`probe-out/`. GitHub Actions artifacts expire and are not reachable from the development sandbox, so
the captures that a fixture is derived from are archived in the repository instead:

    docs/research/data/<kind>-probe-<run-id>/
        <file>.json.gz      original redacted bytes, gzip (mtime 0, deterministic)
        manifest.json       SHA-256 of the raw and gzipped bytes, request records, redaction notes

The raw (uncompressed) file is not committed; `gunzip -c <file>.json.gz > <file>.json` restores it
byte for byte, and `manifest.json` records the SHA-256 to prove it.

Usage:
    archive_probe.py <probe-out> <docs/research/data/player-probe-RUNID> --kind player \
        [--capture-run RUNID] [--source-class live]
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import pathlib
import sys
import time

REDACTION_NOTES = (
    "identity/session/tracking keys removed; cipher blobs dropped; every URL string value has its "
    "query string stripped; bodies that still matched an api-key or signature marker were withheld"
)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def gzip_bytes(data: bytes) -> bytes:
    # mtime=0 keeps the archive byte-identical across runs of the same capture.
    return gzip.compress(data, compresslevel=9, mtime=0)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("probe_out", type=pathlib.Path)
    parser.add_argument("target", type=pathlib.Path)
    parser.add_argument("--kind", required=True, help="search | player")
    parser.add_argument("--capture-run", default="", help="CI run id of the capture")
    parser.add_argument("--source-class", default="live", help="live | synthetic | transformed | third-party")
    args = parser.parse_args()

    if not args.probe_out.is_dir():
        print(f"no probe directory at {args.probe_out}", file=sys.stderr)
        return 2
    args.target.mkdir(parents=True, exist_ok=True)

    summary_path = args.probe_out / "summary.json"
    summary = json.loads(summary_path.read_text(encoding="utf-8")) if summary_path.is_file() else {}
    probe = summary.get("probe", {})
    capture_run = args.capture_run or str(probe.get("run_id") or "")

    files = []
    for source in sorted(args.probe_out.iterdir()):
        if not source.is_file() or source.name.endswith(".gz"):
            continue
        raw = source.read_bytes()
        packed = gzip_bytes(raw)
        gz_name = f"{source.name}.gz"
        (args.target / gz_name).write_bytes(packed)
        files.append(
            {
                "raw_name": source.name,
                "name": gz_name,
                "raw_bytes": len(raw),
                "gz_bytes": len(packed),
                "sha256_raw": sha256(raw),
                "sha256": sha256(packed),
            }
        )

    manifest = {
        "schema": 1,
        "kind": args.kind,
        "capture_run": capture_run,
        "archived_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "captured_at": probe.get("generated_at", ""),
        "commit": probe.get("commit", ""),
        "source_class": args.source_class,
        "captured_by": "vydra provider engine (ReqwestTransport + InnertubeClient, production code path)",
        "redaction": REDACTION_NOTES,
        "restore": "gunzip -c <name>.gz > <raw_name>",
        "probe": probe,
        "requests": summary.get("records", []),
        "files": files,
    }
    (args.target / "manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print(f"archived {len(files)} file(s) to {args.target} (capture run {capture_run or 'unknown'})")
    for entry in files:
        print(f"  {entry['name']} gz={entry['gz_bytes']} raw={entry['raw_bytes']} sha256={entry['sha256'][:16]}…")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
