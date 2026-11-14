#!/usr/bin/env python3
"""Inventory the renderer keys in a probe capture and classify them against the mapper.

The point is to answer, with data rather than opinion, "what does a real search response contain,
and what does Vydra do with each kind of thing in it". The classified set is read from the Rust
mapper itself (`MAPPED` below mirrors the match arms in `crates/vydra-providers/src/innertube/map.rs`
— the test `renderer_inventory_matches_the_mapper` fails if the two drift apart).

Usage:
    python3 scripts/fixtures/inventory_renderers.py probe-out
"""

from __future__ import annotations

import collections
import json
import pathlib
import sys

# Renderers -> a `SearchItem` (map.rs `search_item_from_renderer`).
MAPPED = {"videoRenderer", "channelRenderer", "playlistRenderer"}

# Containers the traversal walks through (map.rs / parse.rs `CONTAINER_KEYS`). They are not
# "unmapped content": they are structure.
CONTAINERS = {
    "contents",
    "continuationItems",
    "twoColumnSearchResultsRenderer",
    "primaryContents",
    "secondaryContents",
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

RENDERER_SUFFIXES = ("Renderer", "ViewModel")


def walk(node: object, found: collections.Counter[str], depth: int = 0) -> None:
    if depth > 40:
        return
    if isinstance(node, dict):
        for key, child in node.items():
            if key.endswith(RENDERER_SUFFIXES) and key not in CONTAINERS:
                found[key] += 1
            walk(child, found, depth + 1)
    elif isinstance(node, list):
        for child in node:
            walk(child, found, depth)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    out = pathlib.Path(sys.argv[1])
    report: dict[str, object] = {"captures": {}}
    for path in sorted(out.glob("search_*.json")):
        if path.name == "summary.json":
            continue
        document = json.loads(path.read_text(encoding="utf-8"))
        found: collections.Counter[str] = collections.Counter()
        walk(document, found)
        mapped = {key: count for key, count in found.items() if key in MAPPED}
        unknown = {
            key: count
            for key, count in sorted(found.items(), key=lambda item: (-item[1], item[0]))
            if key not in MAPPED
        }
        report["captures"][path.stem] = {
            "mapped": mapped,
            "unknown": unknown,
            "unknown_total": sum(unknown.values()),
        }
    report["mapper_mapped_keys"] = sorted(MAPPED)
    print(json.dumps(report, indent=1, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
