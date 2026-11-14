# Fixture provenance

Every file under `crates/vydra-providers/tests/fixtures/` falls into exactly one class. The class is
declared in a `*.meta.json` sidecar next to the fixture and repeated here, because a fixture whose
origin is unclear is worse than no fixture: it invites "the parser matches upstream" claims that the
evidence does not support.

**Phase 5A update.** The player endpoint has its own capture set and its own fixture appendix —
`docs/research/PLAYER_FIXTURE_PROVENANCE.md` — including the redaction rules applied at capture time and
the list of live sections that were *not* obtained (formats, captions, storyboards).

**Phase 4.3 update.** Vydra now has its own captures: on 2026-11-14 it sent five requests to
`/youtubei/v1/search` through the production transport from a GitHub-hosted runner and reduced the
responses into `recorded/search_web_probe_*.json` (class `recorded_vydra_probe`). Everything captured
before that date was third-party. A `recorded_vydra_probe` file is still a *snapshot*: it is not
"what upstream returns today", and its sidecar says so in `not_verified`.

## 1. Classes

| Class | Meaning | What a test may claim |
| --- | --- | --- |
| `recorded_upstream_subset` | A reduced subset of a payload **captured by a third-party project** and published in its test resources. | "Vydra's parser agrees with this recording." Never "upstream answers like this today." |
| `synthetic` | Hand-written document shaped after a documented renderer tree. | "The mapper handles this documented shape." Never "this is what upstream returns." |
| `derived_malformed` | A recorded or synthetic document with one deliberate, described defect. | "The parser degrades in this specific way for this specific defect." |
| `recorded_vydra_probe` | A reduction of a response **Vydra captured itself**, through `ReqwestTransport`, in a CI job whose run id is in the sidecar. | "Vydra's parser handles a document Vydra actually received, on that date, from that network." Never "upstream answers like this now" and never "this proves the contract for every region". |

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
| `recorded/search_web_probe_first_page.json` | recorded_vydra_probe | 208 616 | run `43544223832`, `POST /youtubei/v1/search`, 200, 660 981 B body | 2026-11-14 | `search_parser::recorded_first_page_maps_every_result_and_counts_the_shorts_gap` |
| `recorded/search_web_probe_continuation.json` | recorded_vydra_probe | 196 871 | same run, same endpoint, body `{context, continuation}`, 200, 488 298 B | 2026-11-14 | `search_parser::recorded_continuation_page_maps_every_result` |
| `recorded/search_web_probe_mixed.json` | recorded_vydra_probe | 417 548 | same run, query `veritasium`, 200, 1 236 535 B | 2026-11-14 | `search_parser::recorded_mixed_page_maps_all_kinds_it_can` |
| `recorded/search_web_probe_empty.json` | recorded_vydra_probe | 525 | same run, nonsense query, 200, 21 754 B, `estimatedResults: 0` | 2026-11-14 | `search_parser::recorded_empty_page_is_empty_not_broken` |
| `recorded/search_web_probe_malformed_request.json` | recorded_vydra_probe | 274 | same run, body without `query`, 400 `FAILED_PRECONDITION` | 2026-11-14 | `search_parser::recorded_error_document_yields_no_items` |
| `synthetic/search_mixed.json` | synthetic | 2 139 | hand-written, shaped after `EXTRACTION_METHODS.md` §5.2 | 2026-11-13 | `search_fixture::mixed_search_fixture_maps_to_domain_items` |
| `malformed/search_without_contents.json` | derived | 28 | `browse_channel_search_2023-12-06.json` with `contents` removed (`{"estimatedResults":0}`) | 2026-11-13 | `recorded_fixture::malformed_search_without_contents_is_an_empty_page` |
| `malformed/player_without_playability.json` | derived | 6 652 | `player_web_2025-11-10.json` with `playabilityStatus` removed | 2026-11-13 | `recorded_fixture::malformed_player_without_playability_warns_but_keeps_the_document` |
| `recorded/player_probe_public_unplayable_2026-11-14.json` | recorded_vydra_probe | 12 879 | run `43549736295`, `POST /youtubei/v1/player`, 200, 14 534 B body, `UNPLAYABLE` with metadata | 2026-11-14 | `player_parser::live_unplayable_capture_*` |
| `recorded/player_probe_login_required_2026-11-14.json` | recorded_vydra_probe | 3 536 | same run, bot check (`LOGIN_REQUIRED`), no `videoDetails` | 2026-11-14 | `player_parser::live_login_required_capture_is_a_typed_unavailability` |
| `recorded/player_probe_error_unavailable_2026-11-14.json` | recorded_vydra_probe | 628 | same run, non-existent id (`ERROR`) | 2026-11-14 | `player_parser::live_error_capture_is_a_typed_unavailability_without_inventing_a_reason` |
| `malformed/player_probe_without_playability_2026-11-14.json` | derived | 11 662 | the player probe capture with `playabilityStatus` removed | 2026-11-14 | `player_parser::transformed_capture_without_a_verdict_is_unknown` |
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

### What the probe recordings actually contain

Reduced by `scripts/fixtures/import_probe.py`; the full inventory of what was removed is in each
sidecar (`dropped_renderers`, `reduction`) and the analysis is in
`docs/research/SEARCH_RESPONSE_ANALYSIS.md`.

* **First page** — `contents.twoColumnSearchResultsRenderer.primaryContents.sectionListRenderer.contents[]`:
  one `itemSectionRenderer` with 18 `videoRenderer` and 1 `channelRenderer`, one
  `continuationItemRenderer` (638-char page cursor), plus a `gridShelfViewModel` holding 25
  `shortsLockupViewModel` items with their payload removed. `estimatedResults` 23 544 604.
* **Continuation** (a real page 2) — `onResponseReceivedCommands[].reloadContinuationItemsCommand.continuationItems[]`
  (a different envelope than the first page), 18 `videoRenderer`, a 25-item Shorts shelf, two
  `lockupViewModel` cards, a 670-char cursor, `estimatedResults` 23 544 805.
* **Mixed** — 24 `videoRenderer`, 1 `channelRenderer` (one id appears twice in the page; the mapper
  keeps one and counts the duplicate), 65 `shortsLockupViewModel`, 612-char cursor,
  `estimatedResults` 200 236.
* **Empty** — no renderers at all, `estimatedResults` 0. The live page also carried a
  `backgroundPromoRenderer` ("These results may be inappropriate for some users."); it is chrome and
  is listed as dropped.
* **Malformed request** — `{"error": {code: 400, status: "FAILED_PRECONDITION", …}}` verbatim.

Reproduce:
`python3 scripts/fixtures/import_probe.py probe-out crates/vydra-providers/tests/fixtures
--capture-run <run url>`. The tool refuses to write a fixture if the number of mapped renderers it
keeps disagrees with the count the probe itself reported, so a reduction cannot silently lose results.

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
