#!/usr/bin/env python3
"""Reduce a Phase 5A player capture into committed, replayable fixtures.

Input: an archived capture directory (`docs/research/data/player-probe-<run>/`) produced by
`scripts/fixtures/archive_probe.py`, i.e. redacted bodies plus a manifest with SHA-256 hashes.

Output: fixtures under `crates/vydra-providers/tests/fixtures/` and, next to each one, a
`.meta.json` that states exactly where it came from. Two kinds of output:

* **live** fixtures are the archived bytes, verbatim. The script verifies the SHA-256 against the
  manifest before writing, so a fixture cannot silently drift from its capture.
* **transformed** fixtures are derived from a live body by a named, reproducible edit (recorded in
  its `.meta.json`). They exist because the live capture cannot exercise every branch — e.g. no
  captured document lacks `playabilityStatus` — and inventing a whole document would be fabrication.

Usage:
    import_player_capture.py docs/research/data/player-probe-43549736295 [--check]
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "crates/vydra-providers/tests/fixtures"

# label in the capture -> (fixture path, description)
LIVE = {
    "public": (
        "recorded/player_probe_public_unplayable_2026-11-14.json",
        "Public video answered UNPLAYABLE ('The page needs to be reloaded.') while still carrying "
        "videoDetails and microformat: the hybrid document case.",
    ),
    "captions": (
        "recorded/player_probe_login_required_2026-11-14.json",
        "Public video answered LOGIN_REQUIRED ('Sign in to confirm you're not a bot') with no "
        "videoDetails: the 409 case. Captured from a GitHub runner, so this is the bot check a "
        "datacenter IP receives, not a property of the video.",
    ),
    "missing": (
        "recorded/player_probe_error_unavailable_2026-11-14.json",
        "Syntactically valid id that does not resolve: ERROR / 'This video is unavailable', no "
        "videoDetails. Upstream does not distinguish deleted from never-existed.",
    ),
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=pathlib.Path)
    parser.add_argument("--check", action="store_true", help="verify fixtures match, write nothing")
    args = parser.parse_args()

    manifest_path = args.capture / "manifest.json"
    if not manifest_path.is_file():
        print(f"no manifest at {manifest_path}", file=sys.stderr)
        return 2
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    hashes = {entry["name"]: entry["sha256_raw"] for entry in manifest["files"]}
    probe = manifest.get("probe", {})

    written = 0
    for label, (relative, description) in LIVE.items():
        archive = args.capture / f"player_{label}.json.gz"
        if not archive.is_file():
            print(f"missing capture {archive}", file=sys.stderr)
            return 2
        body = gzip.decompress(archive.read_bytes())
        digest = sha256(body)
        if hashes.get(archive.name) != digest:
            print(
                f"{archive.name}: sha256 {digest} does not match the manifest "
                f"({hashes.get(archive.name)})",
                file=sys.stderr,
            )
            return 1

        record = next(
            (entry for entry in manifest.get("requests", []) if entry.get("label") == label), {}
        )
        target = FIXTURES / relative
        meta = {
            "class": "recorded_vydra_probe",
            "source_class": "live",
            "note": "Captured by Vydra itself through the production transport and client, on a "
            "GitHub Actions runner; archived bytes, unmodified.",
            "captured_by": "vydra-providers example probe_player (GitHub Actions runner)",
            "capture_workflow": "temporary gated CI job; permanent entry point .github/workflows/probe.yml",
            "capture_run": manifest.get("capture_run", ""),
            "captured_at": manifest.get("captured_at", ""),
            "capture_commit": manifest.get("commit", ""),
            "request": {
                "endpoint": "https://www.youtube.com/youtubei/v1/player",
                "query_string": "prettyPrint=false&key=<redacted>",
                "body_keys": record.get("request_body_keys", []),
                "client": {
                    "clientName": probe.get("client_name"),
                    "clientVersion": probe.get("client_version"),
                    "hl": probe.get("hl"),
                    "gl": probe.get("gl"),
                },
                "had_api_key": probe.get("has_api_key"),
                "had_visitor_data": probe.get("has_visitor_data"),
            },
            "http": {
                "status": record.get("status"),
                "bytes": record.get("bytes"),
                "content_type": record.get("content_type"),
                "upstream_date": record.get("upstream_date"),
            },
            "verdict": record.get("verdict", {}).get("playability"),
            "redaction": manifest.get("redaction", ""),
            "transformation": "none — archived bytes, verbatim",
            "sha256": digest,
            "bytes": len(body),
            "description": description,
            "not_verified": [
                "that upstream still answers this way (a snapshot, not a contract)",
                "that every region, account or network sees this verdict",
                "playback URLs, signatures, n-sig or captions content: never captured, never requested",
            ],
        }
        if args.check:
            print(f"ok {relative} sha256={digest[:16]}…")
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(body)
        target.with_suffix(".meta.json").write_text(
            json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        written += 1
        print(f"wrote {relative} ({len(body)} B)")

    # ---- transformed: the same public document without a playability verdict ------------------
    source = gzip.decompress((args.capture / "player_public.json.gz").read_bytes())
    document = json.loads(source)
    document.pop("playabilityStatus", None)
    transformed = (json.dumps(document, indent=2, ensure_ascii=False, sort_keys=True) + "\n").encode()
    relative = "malformed/player_probe_without_playability_2026-11-14.json"
    meta = {
        "class": "derived_malformed",
        "source_class": "transformed",
        "derived_from": "recorded/player_probe_public_unplayable_2026-11-14.json",
        "derivation": "playabilityStatus removed; every other byte of the redacted capture preserved "
        "(keys re-sorted, indentation normalised)",
        "capture_run": manifest.get("capture_run", ""),
        "captured_at": manifest.get("captured_at", ""),
        "transformation_run_by": "scripts/fixtures/import_player_capture.py",
        "sha256": sha256(transformed),
        "bytes": len(transformed),
        "description": "A real player document with the verdict removed: proves a missing verdict "
        "yields Availability::Unknown rather than a fabricated success. Not a capture.",
    }
    target = FIXTURES / relative
    if args.check:
        print(f"ok {relative} sha256={meta['sha256'][:16]}…")
    else:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(transformed)
        target.with_suffix(".meta.json").write_text(
            json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        written += 1
        print(f"wrote {relative} ({len(transformed)} B)")

    print(f"{written} fixture(s) written from capture {manifest.get('capture_run')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
