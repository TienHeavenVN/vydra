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

# Structural renderers: they only group other renderers and must be followed, not dropped. Mirrors
# `STRUCTURAL_RENDERERS` in crates/vydra-providers/src/parse.rs (plus the continuation wrapper, which
# the parser treats as structure because it carries nothing but the token).
STRUCTURAL = {
    "twoColumnSearchResultsRenderer",
    "twoColumnBrowseResultsRenderer",
    "twoColumnWatchNextResults",
    "sectionListRenderer",
    "sectionListContinuationRenderer",
    "itemSectionRenderer",
    "shelfRenderer",
    "horizontalListRenderer",
    "gridRenderer",
    "richGridRenderer",
    "richItemRenderer",
    "expandedShelfContentsRenderer",
    "singleColumnBrowseResultsRenderer",
    "tabRenderer",
    "continuationItemRenderer",
    "reelShelfRenderer",
    "richSectionRenderer",
    "resultSectionRenderer",
    "verticalListRenderer",
    "horizontalCardListRenderer",
    "appendContinuationItemsAction",
    # A Shorts shelf container: `gridShelfViewModel.contents[]` holds `shortsLockupViewModel` items.
    "gridShelfViewModel",
}

# Content shapes that have no mapping yet (a Shorts shelf item, and the newer "lockup" card that is
# a video/playlist presented in the replacement component). Kept, with an empty payload so nothing
# can be inferred from content the mapper does not understand, because the parser counts them as
# dropped results — the fixture has to keep that visible.
CONTENT_VIEW_MODELS = {"shortsLockupViewModel", "lockupViewModel"}

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
    """Copy the document, keeping results and the structure that leads to them.

    Every container is followed generically (the upstream key names for wrappers change between
    experiments, and an enumerated list silently dropped the whole Shorts shelf when it did). What is
    *not* copied: session/tracking material, page chrome (any renderer that is not a mapped one), and
    the payload of any content shape Vydra cannot map yet.
    """

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
        if key in STRUCTURAL:
            out[key] = project(value, stats)
            continue
        if key in CONTENT_VIEW_MODELS:
            out[key] = {}
            stats["unmapped_content"][key] = stats["unmapped_content"].get(key, 0) + 1
            continue
        if key.endswith("Renderer") or key.endswith("ViewModel"):
            # Page chrome (dialogs, menus, filter chips, share panels, per-item menus) or an unknown
            # content shape. Dropped with its subtree: the fixture is about the parse path of the
            # results, and a reduced document with known counters is what lets the tests assert exact
            # numbers. The full inventory of what was dropped goes into the metadata.
            stats["dropped_renderers"][key] = stats["dropped_renderers"].get(key, 0) + 1
            continue
        out[key] = project(value, stats)
    return out


def probe_observation(summary, label):
    mapping = summary.get("probe", {}).get("provider_mapping", {})
    if label != "search_page_1":
        return {"note": "the probe only maps the first capture"}
    return {
        "note": (
            "the probe's own mapping of the unreduced capture, before the page-cursor fix: "
            "next_cursor_len is one of the filter-chip tokens, not the page cursor (F-11)"
        ),
        "items": mapping.get("summary", {}).get("items"),
        "kinds": mapping.get("summary", {}).get("kinds"),
        "parse_warnings": mapping.get("summary", {}).get("parse_warnings"),
        "next_cursor_len": mapping.get("summary", {}).get("next_cursor_len"),
        "estimated_results": mapping.get("summary", {}).get("estimated_results"),
        "ok": mapping.get("ok"),
    }


def build_meta(summary, label, source_bytes, stats, captured_at, capture_run, capture_workflow):
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
        "capture_workflow": capture_workflow,
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
            "content shapes without a mapping kept as empty objects (counted as unmapped_content "
            f"by the parser): {sorted(stats['unmapped_content'])}",
            "page chrome dropped entirely (it is not a result and the parser counts it as "
            f"unknown_renderer only): {sum(stats['dropped_renderers'].values())} occurrence(s) "
            f"across {len(stats['dropped_renderers'])} key(s)",
            f"dropped {stats['dropped']} non-renderer keys that no parser reads",
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
        "capture_run": capture_run,
        "capture_file_bytes": source_bytes,
        # Top-most dropped renderers: a dropped subtree is counted once, so this is an inventory of
        # what was removed, not a count of every nested key. The full per-key inventory of the raw
        # capture is what `scripts/fixtures/inventory_renderers.py` prints.
        "dropped_renderers": dict(
            sorted(stats["dropped_renderers"].items(), key=lambda item: (-item[1], item[0]))
        ),
        "kept_renderers": stats["kept_renderers"],
        # What the probe itself saw when it mapped the *unreduced* capture: the fixture below is a
        # projection, so these live numbers are recorded here rather than being made to look like the
        # fixture's numbers.
        "probe_observation": probe_observation(summary, label),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("probe_out", type=pathlib.Path)
    parser.add_argument("fixtures", type=pathlib.Path)
    parser.add_argument("--captured-at", default=None)
    parser.add_argument(
        "--capture-run",
        default=None,
        help="URL of the workflow run that captured this probe output",
    )
    parser.add_argument(
        "--capture-workflow",
        default=".github/workflows/ci.yml job probe (commit message [probe])",
    )
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

    records = {record["label"]: record for record in summary.get("records", [])}

    recorded = args.fixtures / "recorded"
    recorded.mkdir(parents=True, exist_ok=True)

    written = 0
    for label, stem in targets:
        path = args.probe_out / f"{label}.json"
        if not path.is_file():
            print(f"skip {label}: no capture", file=sys.stderr)
            continue
        document = json.loads(path.read_text(encoding="utf-8"))
        stats = {
            "dropped": 0,
            "tokens": 0,
            "kept_renderers": {},
            "unmapped_content": {},
            "dropped_renderers": {},
        }
        reduced = project(document, stats)
        expected = records.get(label, {})
        for kind, key in (
            ("videoRenderer", "video_renderers"),
            ("channelRenderer", "channel_renderers"),
            ("playlistRenderer", "playlist_renderers"),
        ):
            want = expected.get(key)
            got = stats["kept_renderers"].get(kind, 0)
            if want is not None and want != got:
                print(
                    f"reduction lost {kind}: probe counted {want}, kept {got}",
                    file=sys.stderr,
                )
                return 3
        out_path = recorded / f"{stem}.json"
        out_path.write_text(
            json.dumps(reduced, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        meta = build_meta(
            summary,
            label,
            len(path.read_bytes()),
            stats,
            captured_at,
            args.capture_run,
            args.capture_workflow,
        )
        (recorded / f"{stem}.meta.json").write_text(
            json.dumps(meta, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        written += 1
        print(
            f"{out_path} ({out_path.stat().st_size} B) "
            f"renderers={stats['kept_renderers']} unmapped={sorted(stats['unmapped_content'])} "
            f"chrome_dropped={sum(stats['dropped_renderers'].values())}"
        )

    print(f"wrote {written} fixture(s) from {args.probe_out}")
    return 0 if written else 1


if __name__ == "__main__":
    raise SystemExit(main())
