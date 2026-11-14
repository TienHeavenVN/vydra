# Search response analysis (Phase 4.3)

What a real `POST /youtubei/v1/search` response looks like, what Vydra's parser does with each part of
it, and what it does not understand yet. Everything below comes from documents Vydra captured itself
through the production transport; there is no hand-written sample in this document, and nothing here
is inferred from a third-party client's source.

## 1. Evidence base

| | |
|---|---|
| Capture | `crates/vydra-providers/examples/probe_search.rs`, run through `ReqwestTransport` |
| Runs | `43541388070` (first capture), `43542839419` (first reduction), `43544223832` (capture the fixtures are reduced from; made after the page-cursor fix, so its "page 2" is a real second page) |
| Where | GitHub-hosted runner (the development sandbox has no egress to `youtube.com`) |
| Client | `clientName WEB`, `clientVersion 2.20260805.01.00`, `hl en`, `gl US` |
| Endpoint | `https://www.youtube.com/youtubei/v1/search?prettyPrint=false&key=<redacted>` |
| Fixtures | `crates/vydra-providers/tests/fixtures/recorded/search_web_probe_*.json` (+ `.meta.json`) |
| Inventory tool | `scripts/fixtures/inventory_renderers.py probe-out` |
| Reduction tool | `scripts/fixtures/import_probe.py probe-out crates/vydra-providers/tests/fixtures` |

The five captures of run `43544223832`:

| label | status | bytes | elapsed | items the probe mapped | `estimatedResults` | page cursor |
|---|---|---|---|---|---|---|
| `search_page_1` | 200 | 660 981 | 672 ms | 19 (18 video + 1 channel) | 23 544 604 | 638 chars |
| `search_page_2` | 200 | 488 298 | 609 ms | 18 (18 video) | 23 544 805 | 670 chars |
| `search_mixed` (`veritasium`) | 200 | 1 236 535 | 802 ms | 25 (24 video + 1 channel, one duplicate) | 200 236 | 612 chars |
| `search_empty` | 200 | 21 754 | 1 456 ms | 0 | 0 | none |
| `search_error` | 400 | 274 | 20 ms | 0 | — | — |

The three runs produced different sizes for the same requests (658 813 / 664 211 / 660 981 bytes for
page 1), which is expected: the page carries experiment and personalization state. Every figure below
is from the run the fixtures were reduced from.

**Run history matters here.** `43542839419` "paged" with a filter-chip token (§2.1), so its second
capture is not a second page; `43544223832` ran after the fix, and its `search_page_2` is a real page 2
— 488 298 bytes, the same total estimate as page 1, a further cursor of 670 chars.

## 2. Envelopes: the first page and a continuation are different shapes

First page — the familiar tree:

```
contents
└── twoColumnSearchResultsRenderer
    └── primaryContents
        └── sectionListRenderer
            └── contents[]                      ← item sections, shelf sections, continuation item
                ├── itemSectionRenderer.contents[]
                │   ├── videoRenderer
                │   └── channelRenderer
                └── continuationItemRenderer
                    └── continuationEndpoint.continuationCommand.token   (638 chars)
```

Continuation page — **not** `appendContinuationItemsAction`, which is what the reference clients show
for older browse continuations:

```
onResponseReceivedCommands[]
└── reloadContinuationItemsCommand.continuationItems[]
    └── twoColumnSearchResultsRenderer … → sectionListRenderer.contents[]
        ├── itemSectionRenderer.contents[].videoRenderer
        └── continuationItemRenderer.continuationEndpoint.continuationCommand.token (1 320 chars)
```

Consequences, both already reflected in the code:

* `collect_renderers` (`crates/vydra-providers/src/parse.rs:219`) walks *any* object/array, so it does
  not need to know either envelope. An enumerated list of container keys would have missed
  `reloadContinuationItemsCommand` — and did, in the first version of the fixture importer, which is
  why that tool now follows containers generically too.
* `find_continuation` (`parse.rs:374`) finds the page cursor in both shapes — but only after this
  phase, and only because a token family it used to prefer turned out to be something else (§2.1).

The token is opaque and is round-tripped unchanged; nothing about it is decoded (see
`docs/research/INNERTUBE_REQUEST_CONTRACTS.md` §4 for the request side).

### 2.1 Two families of tokens — and the cursor that was wrong

The first page carries **six** `continuationCommand` tokens: one page cursor (638 chars) on
`sectionListRenderer.contents[].continuationItemRenderer.continuationEndpoint`, and five chips
(746 / 1 018 / 754 / 810 / 812 chars) on
`header.searchHeaderRenderer.chipBar.chipCloudRenderer.chips[n].navigationEndpoint`. The chip labels
were not recorded, which is why lengths are used as identifiers.

They are not interchangeable. A chip token asks upstream for a **filtered or re-sorted** result set;
the page cursor asks for the next page of the same query. `find_continuation` used to return whichever
token its walk met first, which on this page was the 812-char chip token — so `next_cursor` pointed at
a filtered result set while `enable_unverified_filters` is false. The filter path this project keeps
disabled was reachable through the cursor.

That is not a theory. Run `43542839419` recorded `continuation_token_len = 812` for page 1, and its
"page 2" is that chip continuation. Run `43544223832`, made after the fix, shows the difference:

| | chip continuation (run `43542839419`) | page cursor (run `43544223832`) |
|---|---|---|
| cursor used | 812 | 638 |
| response bytes | 263 144 | 488 298 |
| `estimatedResults` | 19 | 23 544 805 (= page 1's total) |
| next cursor | 732 | 670 |

The `estimatedResults` difference is the clearest signal that these are different operations: the page
cursor continues the *query*, the chip token starts a *narrowed* one.

Fixed in this phase: `find_continuation` prefers
`continuationItemRenderer.continuationEndpoint.continuationCommand.token` and falls back to any
`continuationCommand` only when no page cursor exists (`parse.rs:374`). The fixtures pin the page
cursors (638 / 670 / 612), and `a_filter_chip_token_never_becomes_the_page_cursor` guards the
regression.

## 3. Renderer inventory of the captures

Counts are *keys present anywhere in the document* (`inventory_renderers.py`), which is deliberately
wider than what the parser sees: the parser does not descend into a renderer it consumes.

| capture | mapped renderers | content shapes with no mapping | chrome keys | distinct unnamed keys |
|---|---|---|---|---|
| `search_page_1` | 18 `videoRenderer`, 1 `channelRenderer` | 25 `shortsLockupViewModel`, 11 `lockupViewModel` | 851 | 67 |
| `search_page_2` | 18 `videoRenderer` | 25 `shortsLockupViewModel`, 2 `lockupViewModel` | 588 | — |
| `search_mixed` | 24 `videoRenderer`, 1 `channelRenderer` | 65 `shortsLockupViewModel` | 1 461 | — |
| `search_empty` | — | — | 89 | — |

Chrome is everything the parser legitimately does not map and should not pretend to: filter chips
(`searchFilterRenderer` ×25, `searchFilterGroupRenderer` ×5), the hotkey dialog
(`hotkeyDialogSectionOptionRenderer` ×28), per-item menus and overlays (`menuServiceItemRenderer` ×36,
`thumbnailOverlayToggleButtonRenderer` ×36, `menuRenderer` ×18), share panels
(`unifiedSharePanelRenderer` ×28), the topbar (`desktopTopbarRenderer`, `topbarLogoRenderer`,
`topbarMenuButtonRenderer`), and buttons/view-models by the hundred (`buttonViewModel` ×61,
`listItemViewModel` ×73, `thumbnailViewModel` ×61).

The distinction matters and is now encoded in the parser (`parse.rs:111`, `parse.rs:157`):
`unknown_renderer` counts chrome, `unmapped_result` counts **results that were recognised as content
and dropped**. Only the second is a reliability problem.

### 3.1 The Shorts shelf: 25 results that used to vanish

Page 1 contains, at `itemSectionRenderer.contents[14]`:

```
gridShelfViewModel.contents[]      ← a *ViewModel* container, not a *Renderer*
└── shortsLockupViewModel × 25      ← keys that do not end in "Renderer"
```

Before Phase 4.3 the parser produced 19 items and said nothing about those 25: the walk only counted
keys ending in `Renderer`, so a page with 44 visible results reported no loss at all. The mixed page
has 65 of them. They are now listed in `UNMAPPED_CONTENT_VIEW_MODELS` (`parse.rs:70`), counted as
`unmapped_content:shortsLockupViewModel`, and would fail a strict parse.

They are **not mapped** in this phase, and that is a decision rather than an oversight: the item's own
`entityId` is `shorts-shelf-item-<videoId>`, and deriving a video id by string surgery on an
identifier is exactly how a lookalike id becomes a "valid" result. The video id has to come from the
item's reel endpoint in a future capture.

### 3.2 `lockupViewModel`: 11 more

The replacement card component (`lockupViewModel`, 11 on page 1, 1 on the continuation page) is in
`KNOWN_RENDERERS`, so `collect_renderers` hands it to the mapper, which had no arm for it and reported
`unknown_renderer` — the chrome counter. Those items went through the same path in silence. They are
counted as `unmapped_result` now (`map.rs`, `search_item_from_renderer` fallback).

## 4. `estimatedResults` is not a result count

| capture | `estimatedResults` | items parsed |
|---|---|---|
| page 1 | 23 544 604 | 19 |
| page 2 (real continuation) | 23 544 805 | 18 |
| chip continuation (earlier run) | 19 | 18 |
| mixed | 200 236 | 25 |
| empty | 0 | 0 |

On a first page and on a real continuation it is upstream's estimate for the whole query (page 2
repeats page 1's number to within 200). On a *chip* continuation it was 19 for a page with 18 items —
something else entirely, and another reason never to treat the field as a result count or as a paging
signal.
Vydra passes it through unchanged as `SearchResults::estimated_results` with its source text, and no
code path derives paging from it.

## 5. "Empty" is a promo, not an empty tree

`search_empty` (query `zzqqxxvydraprobe9931`) answers **200** (21 754 bytes) with `estimatedResults: 0` and no
results — but its `contents` still contain a section:

```
itemSectionRenderer.contents[]
└── backgroundPromoRenderer
    └── bodyText.runs[] = "These results may be inappropriate for some users."
```

So "no renderers" and "no results" are not the same event, and a caller cannot distinguish "upstream
declined" from "nothing matched" by looking at the item list. Vydra reports 0 items, keeps
`estimatedResults: 0`, and counts the promo as an unknown renderer (`unknown_renderer:
backgroundPromoRenderer`) rather than inventing an error. The reduced fixture keeps the empty tree —
the promo itself is chrome and is listed in the fixture's `.meta.json` under `dropped_renderers`.

## 6. The error document

Sending the context without `query` (the probe does this on purpose) yields:

```
HTTP 400
{"error":{"code":400,"errors":[{"domain":"global","message":"Precondition check failed.",
 "reason":"failedPrecondition"}],"message":"Precondition check failed.","status":"FAILED_PRECONDITION"}}
```

274 bytes, no HTML, no redirect. Two things follow: a malformed *body* is answered with a typed
error rather than an empty page, and the provider maps 4xx to `ProviderError::Unauthorized` (401/403),
`RateLimited` (429) or `Upstream` (everything else) — a 400 therefore surfaces as
`provider_upstream`, not as "zero results". The fixture
`recorded/search_web_probe_malformed_request.json` is that document verbatim.

## 7. Localized text

Nothing is parsed out of the *capture* for localization, because the capture was made with `hl=en`.
The parser's localization behaviour (`crates/vydra-common/src/text.rs`) is covered by hand-written
cases in `tests/search_parser.rs`: `25 399 352 просмотра`, `1,2 Mio. Aufrufe`, `1.2B views`,
`172 371 просмотр`, `No views`, and the clock formats `1:43:17` / `12:34` / `0:07`. Abbreviated
values are marked `Precision::Approximate` and keep their source text in `Count::raw`; exact numbers
stay `Precision::Exact`.

## 8. What is still not verified

* **Shorts and lockup cards.** Recognised, counted, still dropped (§3.1, §3.2).
* **Playlists.** Zero `playlistRenderer` in all five captures, so playlist mapping (including the
  `videoCount` / `videoCountText` fallback added in this phase) is reference-backed only.
* **Region and language.** One capture, `gl=US`, `hl=en`. A different region can serve a different
  tree; the parser's structure assumptions are what the fixtures pin, not the content.
* **Time.** Titles, ids and tokens are a snapshot of 2026-11-14. Tokens expire.
* **Continuation depth.** One hop, and it was a chip continuation (§2.1). A capture of a true page 2 with
  the fixed cursor has not been made yet.
* **Channels results pages, ads.** `officialCardViewModel` appeared once; ad slots did not appear at
  all in these captures, so their shapes are unverified and unmapped.
* **Live `unmapped_content` count.** The probe summary was written before the reclassification, so the
  live number for page 1 is not recorded (the probe reported 19 items / 133 warnings). The inventory
  in §3 and the reduced fixture (which pins 25) are the recorded evidence. A future probe run
  records it directly.
