#!/usr/bin/env python3
"""Reduce a real InnerTube capture to a committable, deterministic fixture.

The corpus in `crates/vydra-providers/tests/fixtures/recorded/` is **not invented**. Each file is a
projection of a response recorded by the MIT-licensed `yuliskov/MediaServiceCore` test suite
(`youtubeapi/src/test/resources/`, commit b3d561f): the JSON is read, telemetry is deleted, and each
content renderer is reduced to the keys the parser consumes. No retained value is rewritten.

Usage:
    python3 scripts/fixtures/extract_recorded.py <MediaServiceCore-checkout> <fixtures-root>

Outputs (fixtures-root is `crates/vydra-providers/tests/fixtures`):
    recorded/browse_channel_search_2023-12-06.json          <- browse/chrome/2023.12.06_channel_search.json
    recorded/continuation_browse_2023-02-23.json <- browse/chrome/2023.02.23_what_to_watch_continuation.json
    recorded/player_web_2025-11-10.json          <- player/web/2025.11.10_player_regular.json
    malformed/*.json                       <- derived deletions, documented in each meta file
"""

from __future__ import annotations

import json
import pathlib
import sys

# Fields the parser reads, per renderer kind. Everything else is dropped: the fixture has to be
# small enough to read in a diff and honest enough to exercise the mapping code.
VIDEO_KEYS = {
    "videoId",
    "title",
    "lengthText",
    "viewCount",
    "viewCountText",
    "shortViewCountText",
    "publishedTimeText",
    "thumbnail",
    "thumbnailOverlays",
    "badges",
    "ownerText",
    "longBylineText",
    "shortBylineText",
}
CHANNEL_KEYS = {
    "channelId",
    "title",
    "navigationEndpoint",
    "descriptionSnippet",
    "thumbnail",
    "subscriberCountText",
    "videoCountText",
}
PLAYLIST_KEYS = {
    "playlistId",
    "title",
    "videoCount",
    "shortBylineText",
    "longBylineText",
    "thumbnail",
}

TEXT_KEYS = {"title", "lengthText", "viewCountText", "shortViewCountText", "publishedTimeText"}

FORMAT_KEYS = {
    "itag",
    "url",
    "mimeType",
    "codecs",
    "bitrate",
    "width",
    "height",
    "fps",
    "qualityLabel",
    "audioQuality",
    "contentLength",
    "approxDurationMs",
    "drmFamilies",
    "signatureCipher",
}

CAPTION_KEYS = {
    "baseUrl",
    "languageCode",
    "kind",
    "vssId",
    "name",
    "isTranslatable",
    "translationLanguages",
}


def strip_query(url: str) -> str:
    """Drop the query string: `sqp`/`expire` parameters are signatures, never reusable."""
    if isinstance(url, str) and url.startswith("http"):
        return url.split("?", 1)[0]
    return url


def project_text(value):
    """Keep the text envelope (`runs`/`simpleText`) but drop tracking from each run."""
    if isinstance(value, dict):
        if "simpleText" in value:
            return {"simpleText": value["simpleText"]}
        if "runs" in value:
            runs = []
            for run in value["runs"]:
                entry = {}
                if "text" in run:
                    entry["text"] = run["text"]
                if "bold" in run:
                    entry["bold"] = run["bold"]
                browse = (
                    run.get("navigationEndpoint", {}).get("browseEndpoint", {})
                    if isinstance(run.get("navigationEndpoint"), dict)
                    else {}
                )
                if browse:
                    entry["navigationEndpoint"] = {
                        "browseEndpoint": {
                            k: browse[k]
                            for k in ("browseId", "canonicalBaseUrl")
                            if k in browse
                        }
                    }
                runs.append(entry)
            return {"runs": runs}
        if "content" in value:
            return {"content": value["content"]}
    return value


def project_thumbnails(value):
    thumbs = (value or {}).get("thumbnails", [])
    return {"thumbnails": [{k: t[k] for k in ("url", "width", "height") if k in t} for t in thumbs]}


def project_video(renderer: dict) -> dict:
    out = {}
    for key in VIDEO_KEYS:
        if key not in renderer:
            continue
        value = renderer[key]
        if key in TEXT_KEYS:
            out[key] = project_text(value)
        elif key == "thumbnail":
            out[key] = project_thumbnails(value)
        elif key == "thumbnailOverlays":
            out[key] = [
                {
                    "thumbnailOverlayTimeStatusRenderer": {
                        "style": overlay["thumbnailOverlayTimeStatusRenderer"].get("style")
                    }
                }
                for overlay in value
                if "thumbnailOverlayTimeStatusRenderer" in overlay
            ]
        elif key == "badges":
            out[key] = [
                {
                    "metadataBadgeRenderer": {
                        k: badge["metadataBadgeRenderer"][k]
                        for k in ("label", "style")
                        if k in badge.get("metadataBadgeRenderer", {})
                    }
                }
                for badge in value
                if "metadataBadgeRenderer" in badge
            ]
        elif key in ("ownerText", "longBylineText", "shortBylineText"):
            out[key] = project_text(value)
        else:
            out[key] = value
    return out


def project_channel(renderer: dict) -> dict:
    out = {}
    for key in CHANNEL_KEYS:
        if key not in renderer:
            continue
        value = renderer[key]
        if key in ("title", "subscriberCountText", "videoCountText", "descriptionSnippet"):
            out[key] = project_text(value)
        elif key == "thumbnail":
            out[key] = project_thumbnails(value)
        elif key == "navigationEndpoint":
            browse = value.get("browseEndpoint", {})
            out[key] = {
                "browseEndpoint": {
                    k: browse[k] for k in ("browseId", "canonicalBaseUrl") if k in browse
                }
            }
        else:
            out[key] = value
    return out


def project_playlist(renderer: dict) -> dict:
    out = {}
    for key in PLAYLIST_KEYS:
        if key not in renderer:
            continue
        value = renderer[key]
        if key in ("title", "shortBylineText", "longBylineText"):
            out[key] = project_text(value)
        elif key == "thumbnail":
            out[key] = project_thumbnails(value)
        else:
            out[key] = value
    return out


def project_renderer(node: dict) -> dict:
    """Project a wrapper object such as `{"videoRenderer": {...}}`."""
    for kind, keys in (
        ("videoRenderer", None),
        ("channelRenderer", None),
        ("playlistRenderer", None),
        ("gridVideoRenderer", None),
    ):
        if kind in node:
            renderer = node[kind]
            if kind in ("videoRenderer", "gridVideoRenderer"):
                return {kind: project_video(renderer)}
            if kind == "channelRenderer":
                return {kind: project_channel(renderer)}
            return {kind: project_playlist(renderer)}
    if "continuationItemRenderer" in node:
        token = (
            node["continuationItemRenderer"]
            .get("continuationEndpoint", {})
            .get("continuationCommand", {})
            .get("token")
        )
        if token:
            return {
                "continuationItemRenderer": {
                    "continuationEndpoint": {"continuationCommand": {"token": token}}
                }
            }
    return {}


def first_search_tab(contents: dict):
    for wrapper in contents.get("twoColumnBrowseResultsRenderer", {}).get("tabs", []):
        tab = wrapper.get("expandableTabRenderer")
        if tab and "content" in tab:
            return tab["content"]
    return None


def build_search(source: pathlib.Path) -> dict:
    payload = json.loads(source.read_text())
    content = first_search_tab(payload.get("contents", {}))
    if content is None:
        raise SystemExit(f"{source}: no expandable tab with content")
    sections = []
    for section in content["sectionListRenderer"]["contents"]:
        items = section.get("itemSectionRenderer", {}).get("contents", [])
        kept = [projected for projected in (project_renderer(item) for item in items) if projected]
        if kept:
            sections.append({"itemSectionRenderer": {"contents": kept}})
        elif "continuationItemRenderer" in section:
            projected = project_renderer(section)
            if projected:
                sections.append(projected)
    if not sections:
        raise SystemExit(f"{source}: no content renderers survived projection")
    sections = sections[:6]
    return {
        "contents": {
            "twoColumnBrowseResultsRenderer": {
                "tabs": [
                    {
                        "expandableTabRenderer": {
                            "content": {"sectionListRenderer": {"contents": sections}}
                        }
                    }
                ]
            }
        }
    }


def build_player(source: pathlib.Path) -> dict:
    payload = json.loads(source.read_text())
    streaming = payload.get("streamingData", {})

    def project_formats(formats, limit):
        kept = []
        for fmt in formats[:limit]:
            entry = {}
            for key in FORMAT_KEYS:
                if key not in fmt:
                    continue
                entry[key] = strip_query(fmt[key]) if key == "url" else fmt[key]
            kept.append(entry)
        return kept

    details = payload.get("videoDetails", {})
    micro = payload.get("microformat", {}).get("playerMicroformatRenderer", {})
    tracks = (
        payload.get("captions", {})
        .get("playerCaptionsTracklistRenderer", {})
        .get("captionTracks", [])
    )
    kept_tracks = []
    for track in tracks[:2]:
        kept_track = {}
        for key in CAPTION_KEYS:
            if key not in track:
                continue
            kept_track[key] = strip_query(track[key]) if key == "baseUrl" else track[key]
        kept_track["name"] = project_text(track.get("name"))
        kept_tracks.append(kept_track)

    return {
        "playabilityStatus": {
            k: v for k, v in payload.get("playabilityStatus", {}).items() if k in ("status", "reason")
        },
        "videoDetails": {
            k: details[k]
            for k in (
                "videoId",
                "title",
                "lengthSeconds",
                "viewCount",
                "channelId",
                "author",
                "shortDescription",
                "isLiveContent",
                "isLiveNow",
                "isUpcoming",
                "keywords",
            )
            if k in details
        },
        "microformat": {
            "playerMicroformatRenderer": {
                k: micro[k] for k in ("publishDate", "uploadDate", "category", "lengthSeconds") if k in micro
            }
            | {
                "thumbnail": project_thumbnails(micro.get("thumbnail"))
                if "thumbnail" in micro
                else {"thumbnails": []}
            }
        },
        "streamingData": {
            "expiresInSeconds": streaming.get("expiresInSeconds"),
            "formats": project_formats(streaming.get("formats", []), 1),
            "adaptiveFormats": project_formats(streaming.get("adaptiveFormats", []), 3),
        },
        "captions": {
            "playerCaptionTracklistPlaceholder": None,
            "playerCaptionsTracklistRenderer": {"captionTracks": kept_tracks},
        },
    }


def build_continuation(source: pathlib.Path) -> dict:
    """Project an `appendContinuationItemsAction` response (page 2 of a feed)."""
    payload = json.loads(source.read_text())
    action = payload["onResponseReceivedActions"][0]["appendContinuationItemsAction"]
    items = []
    for item in action.get("continuationItems", []):
        rich = item.get("richItemRenderer")
        if rich:
            projected = project_renderer(rich.get("content", {}))
            if projected:
                items.append({"richItemRenderer": {"content": projected}})
            continue
        projected = project_renderer(item)
        if projected:
            items.append(projected)
    # Keep the first five content items plus the trailing continuation item: the token is what the
    # next page request is built from, so dropping it would make the fixture useless for pagination.
    content = [item for item in items if "richItemRenderer" in item][:5]
    tail = [item for item in items if "continuationItemRenderer" in item][:1]
    return {
        "onResponseReceivedActions": [
            {"appendContinuationItemsAction": {"continuationItems": content + tail}}
        ]
    }


def drop(mapping: dict, *keys: str) -> dict:
    return {k: v for k, v in mapping.items() if k not in keys}


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    root = pathlib.Path(sys.argv[1]) / "youtubeapi/src/test/resources"
    fixtures = pathlib.Path(sys.argv[2])
    recorded = fixtures / "recorded"
    malformed = fixtures / "malformed"
    recorded.mkdir(parents=True, exist_ok=True)
    malformed.mkdir(parents=True, exist_ok=True)

    search = build_search(root / "browse/chrome/2023.12.06_channel_search.json")
    (recorded / "browse_channel_search_2023-12-06.json").write_text(json.dumps(search, indent=2) + "\n")

    continuation = build_continuation(
        root / "browse/chrome/2023.02.23_what_to_watch_continuation.json"
    )
    (recorded / "continuation_browse_2023-02-23.json").write_text(
        json.dumps(continuation, indent=2) + "\n"
    )

    player = build_player(root / "player/web/2025.11.10_player_regular.json")
    player["captions"] = {"playerCaptionsTracklistRenderer": player["captions"]["playerCaptionsTracklistRenderer"]}
    (recorded / "player_web_2025-11-10.json").write_text(json.dumps(player, indent=2) + "\n")

    # Malformed cases are *deletions* from the recorded files, never edits of retained values.
    (malformed / "search_without_contents.json").write_text(
        json.dumps({"estimatedResults": 0}, indent=2) + "\n"
    )
    (malformed / "player_without_playability.json").write_text(
        json.dumps({"videoDetails": player["videoDetails"]}, indent=2) + "\n"
    )
    truncated = json.dumps(search, indent=2)
    (malformed / "search_truncated.json").write_text(truncated[: len(truncated) // 2] + "\n")

    print("search fixture bytes:", (recorded / "browse_channel_search_2023-12-06.json").stat().st_size)
    print("continuation fixture bytes:", (recorded / "continuation_browse_2023-02-23.json").stat().st_size)
    print("player fixture bytes:", (recorded / "player_web_2025-11-10.json").stat().st_size)


if __name__ == "__main__":
    main()
