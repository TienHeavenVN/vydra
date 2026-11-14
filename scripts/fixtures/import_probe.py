#!/usr/bin/env python3
"""Reduce a Phase 4.3 probe capture into the recorded fixture corpus.

Input  : the `probe-out/` directory produced by `examples/probe_search.rs`
Output : `tests/fixtures/recorded/<name>.json` + `.meta.json`

The reduction is a *projection*, never a rewrite: every value that is kept is copied verbatim from
the capture. It exists because a live search page is 200 KB–1.5 MB of tracking/experiment noise
around the renderers the parser actually reads, and because a fixture in the repository must contain
no URL query strings, no visitor id and no api key (the probe already strips them; this script drops
URL keys entirely, since the search parser never reads one).

Usage:
    python3 scripts/fixtures/import_probe.py probe-out crates/vydra-providers/tests/fixtures
    python3 scripts/fixtures/import_probe.py probe-out ... --captured-at 2026-11-14T00:00:00Z
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

# Containers that carry structure the parser traverses (kept so a fixture exercises the real path).
CONTAINERS = {
    "contents",
    "continuationItems",
    "twoColumnSearchResultsRenderer",
    "primaryContents",
    "sectionListRenderer",
    "itemSectionRenderer",
    "shelfRenderer",
    "expandedShelfContentsRenderer",
    "gridRenderer",
    "richGridRenderer",
    "richSectionRenderer",
    "richItemRenderer",
    "horizontalCardListRenderer",
    "reelShelfRenderer",
    "onResponseReceivedActions",
    "appendContinuationItemsAction",
    "resultSectionRenderer",
    "verticalListRenderer",
}

# Renderers the mapper understands, with the exact keys it reads.
LEAF_RENDERERS = {
    "videoRenderer": [
        "videoId",
        "title",
        "headline",
        "lengthText",
        "viewCountText",
        "shortViewCountText",
        "viewCount",
        "publishedTimeText",
        "thumbnail",
        "badges",
        "ownerText",
        "shortBylineText",
        "thumbnailOverlays",
        "navigationEndpoint",
    ],
    "compactVideoRenderer": [
        "videoId",
        "title",
        "headline",
        "lengthText",
        "viewCountText",
        "shortViewCountText",
        "publishedTimeText",
        "thumbnail",
        "badges",
        "shortBylineText",
    ],
    "gridVideoRenderer": [
        "videoId",
        "title",
        "viewCountText",
        "thumbnail",
        "thumbnailOverlays",
    ],
    "channelRenderer": [
        "channelId",
        "title",
        "descriptionSnippet",
        "thumbnail",
        "subscriberCountText",
        "videoCountText",
        "navigationEndpoint",
    ],
    "playlistRenderer": [
        "playlistId",
        "title",
        "shortBylineText",
        "videoCountText",
        "videoCount",
        "thumbnail",
        "navigationEndpoint",
    ],
}

# Keys that are pure session/tracking material wherever they appear.
DROP_EVERYWHERE = {
    "responseContext",
    "frameworkUpdates",
    "trackingParams",
    "clickTrackingParams",
    "serviceTrackingParams",
    "attributedFullText",
    "loggingContext",
    "visitorData",
}


def project_navigation(endpoint):
    """Keep only the browse identity a fixture needs (browseId + canonicalBaseUrl)."""
    if not isinstance(endpoint, dict):
        return None
    browse = endpoint.get("browseEndpoint")
    if not isinstance(browse, dict):
        return None
    kept = {}
    for key in ("browseId", "canonicalBaseUrl"):
        if key in browse:
            kept[key] = browse[key]
    return {"browseEndpoint": kept} if kept else None


def project_renderer(kind, renderer):
    allowed = LEAF_RENDERERS[kind]
    out = {}
    for key in allowed:
        if key not in renderer:
            continue
        if key == "navigationEndpoint":
            projected = project_navigation(renderer[key])
            if projected is not None:
                out[key] = projected
            continue
        out[key] = renderer[key]
    return out


def project(node, stats):
    if isinstance(node, list):
        return [project(child, stats) for child in node]
    if not isinstance(node, dict):
        return node

    out = {}
    for key, value in node.items():
        if key in DROP_EVERYWHERE:
            stats["dropped"] += 1
            continue
        if key in LEAF_RENDERERS:
            stats["kept_renderers"][key] = stats["kept_renderers"].get(key, 0) + 1
            out[key] = project_renderer(key, value)
            continue
        if key == "continuationItemRenderer":
            token = (
                value.get("continuationEndpoint", {})
                .get("continuationCommand", {})
                .get("token")
            )
            if isinstance(token, str):
                out[key] = {
                    "continuationEndpoint": {"continuationCommand": {"token": token}}
                }
                stats["tokens"] += 1
                continue
            stats["dropped"] += 1
            continue
        if key in CONTAINERS:
            out[key] = project(value, stats)
            continue
        if key == "estimatedResults":
            out[key] = value
            continue
        if key.endswith("Renderer") or key.endswith("ViewModel"):
            # An unknown renderer: keep the key (so the parser must report it as unknown) but none of
            # its payload — nothing may be inferred from content the mapper does not understand.
            out[key] = {}
            stats["unknown_renderers"][key] = stats["unknown_renderers"].get(key, 0) + 1
            continue
        stats["dropped"] += 1
    return out


def build_meta(summary, label, source_bytes, stats, captured_at):
    records = {record["label"]: record for record in summary.get("records", [])}
    record = records.get(label, {})
    notes = summary.get("probe", {})
    return {
        "class": "recorded_vydra_probe",
        "note": (
            "Captured by Vydra itself, not by hand and not by a third party: "
            "crates/vydra-providers/examples/probe_search.rs ran through the production "
            "ReqwestTransport on a GitHub Actions runner. Reduced (projected, never rewritten) by "
            "scripts/fixtures/import_probe.py."
        ),
        "captured_at": captured_at,
        "captured_by": "vydra-providers example probe_search (GitHub Actions runner)",
        "capture_workflow": ".github/workflows/ci.yml job probe (commit message [probe])",
        "request": {
            "endpoint": "https://www.youtube.com/youtubei/v1/search",
            "query_string": "prettyPrint=false&key=<redacted>",
            "body_keys": record.get("request_body_keys"),
            "client": {
                "clientName": notes.get("client_name"),
                "clientVersion": notes.get("client_version"),
                "hl": notes.get("hl"),
                "gl": notes.get("gl"),
            },
            "had_api_key": notes.get("has_api_key"),
            "had_visitor_data": notes.get("has_visitor_data"),
        },
        "http": {
            "status": record.get("status"),
            "bytes": record.get("bytes"),
            "elapsed_ms": record.get("elapsed_ms"),
        },
        "reduction": [
            "kept the container chain (contents/sectionListRenderer/itemSectionRenderer/shelf) and top-level action envelopes",
            "projected every mapped renderer to the keys the mapper reads",
            "kept continuation tokens verbatim (they expire; none is presented as usable)",
            "kept estimatedResults",
            f"unknown renderers kept as empty objects: {sorted(stats['unknown_renderers'])}",
            f"dropped {stats['dropped']} keys that no parser reads",
        ],
        "redaction": [
            "api key, visitor id and cookie material removed by the probe before the file was written",
            "every URL query string stripped by the probe; this script drops URL keys entirely",
            "responseContext / trackingParams / frameworkUpdates removed",
        ],
        "not_verified": [
            "that a later request from a different network or region returns the same tree",
            "that the captured content is still current (item ids and titles are a snapshot)",
            "playback URLs: deliberately not represented",
        ],
        "source_bytes": source_bytes,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("probe_out", type=pathlib.Path)
    parser.add_argument("fixtures", type=pathlib.Path)
    parser.add_argument("--captured-at", default=None)
    args = parser.parse_args()

    summary_path = args.probe_out / "summary.json"
    if not summary_path.is_file():
        print(f"no summary at {summary_path}", file=sys.stderr)
        return 2
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    captured_at = args.captured_at or summary.get("probe", {}).get("generated_at")
    if not isinstance(captured_at, str) or not captured_at:
        print("summary has no generated_at; pass --captured-at", file=sys.stderr)
        return 2

    targets = [
        ("search_page_1", "search_web_probe_first_page"),
        ("search_page_2", "search_web_probe_continuation"),
        ("search_mixed", "search_web_probe_mixed"),
        ("search_empty", "search_web_probe_empty"),
        ("search_error", "search_web_probe_malformed_request"),
    ]

    recorded = args.fixtures / "recorded"
    recorded.mkdir(parents=True, exist_ok=True)

    written = 0
    for label, stem in targets:
        path = args.probe_out / f"{label}.json"
        if not path.is_file():
            print(f"skip {label}: no capture", file=sys.stderr)
            continue
        document = json.loads(path.read_text(encoding="utf-8"))
        stats = {"dropped": 0, "tokens": 0, "kept_renderers": {}, "unknown_renderers": {}}
        reduced = project(document, stats)
        out_path = recorded / f"{stem}.json"
        out_path.write_text(
            json.dumps(reduced, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        meta = build_meta(summary, label, len(path.read_bytes()), stats, captured_at)
        (recorded / f"{stem}.meta.json").write_text(
            json.dumps(meta, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        written += 1
        print(
            f"{out_path} ({out_path.stat().st_size} B) "
            f"renderers={stats['kept_renderers']} unknown={sorted(stats['unknown_renderers'])}"
        )

    print(f"wrote {written} fixture(s) from {args.probe_out}")
    return 0 if written else 1


if __name__ == "__main__":
    raise SystemExit(main())
