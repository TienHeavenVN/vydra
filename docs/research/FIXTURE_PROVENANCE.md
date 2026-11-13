# Fixture provenance

Every file under `crates/vydra-providers/tests/fixtures/` falls into exactly one class. The class is
declared in a `*.meta.json` sidecar next to the fixture and repeated here, because a fixture whose
origin is unclear is worse than no fixture: it invites "the parser matches upstream" claims that the
evidence does not support.

**Vydra has never captured a fixture itself.** No request in this repository was ever sent to YouTube.
Nothing here is labelled "live", and no recorded file is presented as current.

## 1. Classes

| Class | Meaning | What a test may claim |
| --- | --- | --- |
| `recorded_upstream_subset` | A reduced subset of a payload **captured by a third-party project** and published in its test resources. | "Vydra's parser agrees with this recording." Never "upstream answers like this today." |
| `synthetic` | Hand-written document shaped after a documented renderer tree. | "The mapper handles this documented shape." Never "this is what upstream returns." |
| `derived_malformed` | A recorded or synthetic document with one deliberate, described defect. | "The parser degrades in this specific way for this specific defect." |

Rules that were applied when the corpus was built:

1. **No fabricated recorded data.** A `recorded_upstream_subset` file is produced only by
   `scripts/fixtures/extract_recorded.py` from a checked-out upstream repository. Values are copied
   verbatim; the script may remove keys and may truncate lists, never rewrite a value.
2. **Synthetic stays separate.** Synthetic documents live under `synthetic/` and are never described
   as captures, in tests, in comments or in reports.
3. **Non-sensitive only.** Captures are public, unauthenticated content. Every URL query string is
   stripped (no `sig`, no `expire`, no PoToken, no visitor-bound parameter), and no cookie, token or
   account identifier is retained.
4. **Structure preserved.** Text envelopes (`simpleText`, `runs`) and browse ids are kept as recorded,
   because the parser must deal with the real nesting, not a flattened convenience shape.
5. **The gap is written down.** Every sidecar has a `not_verified` list. If a reader wants to know what
   a test does *not* prove, that list is the answer.

## 2. The corpus

| File | Class | Bytes | Upstream source | Captured | Used by |
| --- | --- | --- | --- | --- | --- |
| `recorded/browse_channel_search_2023-12-06.json` | recorded subset | 29 230 | `yuliskov/MediaServiceCore` @ `b3d561f` — `youtubeapi/src/test/resources/browse/chrome/2023.12.06_channel_search.json` (MIT) | 2023-12-06 | `recorded_fixture::recorded_browse_page_maps_every_renderer` |
| `recorded/continuation_browse_2023-02-23.json` | recorded subset | 19 245 | `yuliskov/MediaServiceCore` @ `b3d561f` — `youtubeapi/src/test/resources/browse/chrome/2023.02.23_what_to_watch_continuation.json` (MIT) | 2023-02-23 | `recorded_fixture::recorded_continuation_page_keeps_the_trailing_token` |
| `recorded/player_web_2025-11-10.json` | recorded subset | 8 963 | `yuliskov/MediaServiceCore` @ `b3d561f` — `youtubeapi/src/test/resources/player/web/2025.11.10_player_regular.json` (MIT) | 2025-11-10 | `recorded_fixture::recorded_player_page_maps_availability_formats_and_captions` |
| `synthetic/search_mixed.json` | synthetic | 2 139 | hand-written, shaped after `EXTRACTION_METHODS.md` §5.2 | 2026-11-13 | `search_fixture::mixed_search_fixture_maps_to_domain_items` |
| `malformed/search_without_contents.json` | derived | 28 | `browse_channel_search_2023-12-06.json` with `contents` removed (`{"estimatedResults":0}`) | 2026-11-13 | `recorded_fixture::malformed_search_without_contents_is_an_empty_page` |
| `malformed/player_without_playability.json` | derived | 6 652 | `player_web_2025-11-10.json` with `playabilityStatus` removed | 2026-11-13 | `recorded_fixture::malformed_player_without_playability_warns_but_keeps_the_document` |
| `malformed/search_truncated.json` | derived | 14 615 | `browse_channel_search_2023-12-06.json` truncated mid-document (invalid JSON) | 2026-11-13 | `recorded_fixture::truncated_search_document_is_a_parse_error` |

All three recordings come from the same upstream commit, so the corpus is internally consistent about
which client generation it represents.

### What the recordings actually contain

* **Browse (channel search tab)** — `twoColumnBrowseResultsRenderer.tabs[0].expandableTabRenderer.content.sectionListRenderer.contents[].itemSectionRenderer.contents[]`
  with 6 items (5 `videoRenderer`, 1 `playlistRenderer`), Russian locale, `U+00A0` thousands
  separators (`"25 399 352 просмотра"`), no continuation token.
* **Browse continuation** — `onResponseReceivedActions[0].appendContinuationItemsAction.continuationItems`
  with 5 `richItemRenderer.content.videoRenderer` items plus one trailing
  `continuationItemRenderer` carrying a 582-character token bound to `/youtubei/v1/browse`. The token
  is expired; nothing in the tests presents it.
* **Player (web)** — `playabilityStatus.status = "OK"`, `videoDetails` (`9Umm2MAKsMc`, 3537 s,
  31 834 views, channel `UCTk3OjLsX-sYl60JBq3z5QQ`, Russian description), 1 muxed `formats` entry and
  3 `adaptiveFormats` entries, 1 caption track (`ru`, `asr`), `microformat` publishDate `2025-11-09`
  and category `Gaming`.

## 3. What remains unverified

| Claim | Status | Why |
| --- | --- | --- |
| Global `/youtubei/v1/search` container path | **unverified** | no recording of a global search response was found; the browse capture only proves the leaf renderers |
| Search continuation envelope | **unverified** | only a browse continuation was available |
| Current response shapes | **unverified** | recordings are 1–3 years old |
| Playback URL usability | **out of scope** | query strings are stripped by policy; Vydra never returns URLs |
| Signature / `n-sig` / PoToken behaviour | **out of scope** | deliberately not implemented |
| Bootstrap document (`sw.js_data`) | **synthetic only** | the bootstrap tests use a hand-written document in the documented shape |

Each of these is also stated in the `not_verified` array of the relevant sidecar, so a reader who
opens a fixture cannot miss it.

## 4. Reproducing the corpus

```bash
git clone https://github.com/yuliskov/MediaServiceCore /tmp/MediaServiceCore
git -C /tmp/MediaServiceCore checkout b3d561f53cedca04d642b6e7f914f19b4536ce69
python3 scripts/fixtures/extract_recorded.py /tmp/MediaServiceCore \
        crates/vydra-providers/tests/fixtures
```

The script is deterministic: it walks a fixed list of upstream files, projects renderers to the keys
the parser reads, caps list lengths, strips every URL query string, and writes both the fixture and
nothing else. It never writes into `synthetic/`. Re-running it on the pinned commit reproduces the
three recorded files byte-for-byte; if it does not, the script or the corpus has drifted and the
difference must be explained before the fixtures are trusted again.

## 5. Adding a fixture

1. Find a source whose licence permits reuse (MIT/Unlicense in the sources examined; GPL/AGPL
   projects were read for behaviour only and contributed no fixture bytes).
2. Record repository, commit SHA, path, licence and capture date in the sidecar.
3. Reduce with a script; never hand-edit a recorded value.
4. Add a `not_verified` list — even if it only says "that this shape is still current".
5. Reference the sidecar from the test that consumes the file.
