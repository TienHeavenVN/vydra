# Channel contract

What Vydra claims about a channel, where each claim comes from, and what it refuses to claim.

Normative code: `crates/vydra-domain/src/channel.rs` (the document), `crates/vydra-providers/src/innertube/browse_map.rs`
(the mapper), `crates/vydra-providers/src/innertube/request.rs` (the typed requests). Fixtures and their provenance:
`docs/research/CHANNEL_PLAYLIST_FIXTURE_PROVENANCE.md`. Verification boundaries: `docs/reports/PHASE_6_REPORT.md` §7.

## 1. What a channel document is, and is not

`GET /api/v1/channels/{id}` returns one `ChannelDocument`. It is a statement about *the page Vydra read*, not about
the channel in the abstract:

* every field is accompanied by `provenance` (provider, endpoint, client, fetch time, parse warnings, provider
  attempts) and, when more than one section was read, by `evidence` naming the sections that supplied values,
  the fallbacks that were needed and any conflicts that were recorded;
* a field the page did not state is **absent**, never `0`, never `""`, never `"unknown"`;
* nothing is derived from a formatted string without recording that it is derived: `21.4M subscribers` becomes
  `{value: 21400000, precision: "approximate", raw: "21.4M subscribers"}`, and an exact count carries no `raw`
  because there is no rendering to preserve.

The document deliberately does **not** contain: a subscriber count inferred from a handle, a channel id derived from
a handle, a view count the page did not render, a country the page did not state, or links scraped from an `about`
tab this phase never requests.

## 2. The wire requests

| Step | Body | When |
| --- | --- | --- |
| channel page | `{context, browseId: "UC…"}` | always, one `browse` call |
| videos tab, page 1 | `{context, browseId: "UC…", params: <relayed>}` | only for `GET /channels/{id}/videos` |
| videos tab, page *n* | `{context, continuation: <token>}` | only when a genuine page token was found |

`context` is built once from the client profile and the session (`ClientContext::new`), so `hl`/`gl` reach every
request. `params` is the only encoded blob Vydra ever sends, and it is never constructed:

* the value is copied from `tabRenderer.endpoint.browseEndpoint.params` on the channel page itself;
* it is accepted only if `is_relayable_params` passes (non-empty, ≤ 512 bytes, ASCII alphanumerics plus `-`, `_`,
  `=`) — a filter chip's token is longer, contains `%` escapes and is refused;
* no client-side table of tab `params` exists anywhere in the tree, because a hard-coded blob is a claim about
  upstream that Vydra cannot verify.

A handle is **not** sent: see §7.

## 3. Fields and their sources

| Field | Source | Absent means |
| --- | --- | --- |
| `id` | `channelMetadataRenderer.externalId`, cross-checked against the requested id | a mismatch is a `FieldConflict` and the *canonical* id is kept; a page with no id is a parse failure |
| `title` | `channelMetadataRenderer.title`, else the header title | a disagreement is recorded in `evidence.conflicts`, metadata wins |
| `handle` | `channelMetadataRenderer.vanityChannelUrl` | the page renders no vanity URL |
| `description` | `channelMetadataRenderer.description` | no description on the page |
| `thumbnails` | header avatar (`decoratedAvatarViewModel` / `avatarViewModel`), falling back to `channelMetadataRenderer.avatar` | no avatar |
| `banner` | `pageHeaderViewModel.banner.imageBannerViewModel.image.sources`, legacy `c4TabbedHeaderRenderer.banner.thumbnails` | system channels (auto-generated artist channels) render no banner |
| `subscriber_count` | header metadata row (see §4) | the page did not state one, or the row was ambiguous |
| `video_count` | header metadata row (see §4) | as above |
| `total_view_count` | nothing yet (`about` tab not requested) | absent |
| `country`, `joined_at`, `links` | nothing yet (`about` tab not requested) | absent |
| `tabs` | `tabRenderer` titles plus their `params` | the page has no tab strip |
| `verification` | `metadataBadgeRenderer` / `badgeViewModel` styles under the header | no badge rendered |
| `availability` | alerts only (`alertRenderer` with a `WARNING`/`ERROR` style) | the page opened normally |

## 4. Counts: positional, always approximate, never guessed

The page header renders counts as localized metadata *parts* ("21.4M subscribers", "1.1K videos", "Abonnenten",
"vidéos"), so a keyword match would work in one locale and mislabel another. Vydra therefore reads the **last**
metadata row and counts how many of its parts parse as a count:

* **two** count-bearing parts → first is subscribers, second is videos. This positional reading is what
  `NewPipeExtractor` documents ("the subscriber count is at the same position for all languages as of 07/07/2025");
  because the value came from a human-formatted label and a positional rule, it is always `approximate` and keeps the
  rendered text in `raw`, so a reader can see the label the position was applied to;
* **one** count-bearing part → **neither** field is set, and `channel_counts_ambiguous` is recorded as a parse
  warning. A single count on an artist channel is the video count; on a hidden-subscriber channel it is the video
  count; on another surface it could be either. Vydra does not guess;
* **zero** → both absent, no claim.

The live probe (§9) read `subscriber_count = 21 400 000 (approximate, raw "21.4M subscribers")` and
`video_count = 550 (approximate)` from one real channel in the two-part case, which is the case the positional rule
is designed for.

## 5. Tabs

Tabs come from the page's tab strip in **document order**. A tab is emitted with its rendered title always, and with
a `slug` only when the title is one Vydra recognizes (`home`, `videos`, `shorts`, `live`, `playlists`, `podcasts`,
`releases`, `store`, `about`, …). An unrecognized title keeps its text and no slug: the API never invents a slug, and
`GET /channels/{id}/videos` selects the tab whose slug is `videos` rather than trusting a position.

`selected` mirrors the page, and is not used for selection: a client must not infer "this is the videos tab" from it.

## 6. Verification indicators

Badges are read as rendered: `{style, label}` pairs, the style string kept verbatim (`VERIFIED`, `CHECK_CIRCLE_THICK`,
`OFFICIAL_ARTIST_BADGE`, …). Vydra does not map styles to an enum with a meaning, because the meaning of a style is
upstream's, not Vydra's. A channel with no badge renders an empty list, which is different from a channel known not
to be verified — the page simply said nothing.

## 7. Handles are refused, not resolved

`GET /api/v1/channels/{id}` accepts a canonical `UC…` id only. A `@handle` is rejected with
`400 validation_failed{field: "id", issue: "handle_not_supported"}`, and the provider refuses it with a typed
`Unsupported` error as defence in depth.

This is a **measured** decision, not a preference. The Phase 6 probe sent `{browseId: "@veritasium"}` and received
`HTTP 400` from upstream (run 43561555917, 2026-11-13). Resolving a handle would therefore require a second request
(a search, or an `about` tab) whose result is a guess about which channel the caller meant, and would make the
response's `id` differ from the id the client asked about. Vydra would rather return a clear 400 than a guess.

The domain still models handles (`ChannelRef::Handle`, `ChannelSummary::handle`) because a page states its own vanity
URL and a search result may carry one; what is refused is *using* a handle as an input.

## 8. Listings: rows, pagination, duplicates

`GET /api/v1/channels/{id}/videos` returns `ChannelVideoList`:

| Concept | Rule |
| --- | --- |
| rows | `richGridRenderer`/`itemSectionRenderer` contents are walked for `videoRenderer`, `gridVideoRenderer`, `lockupViewModel`; anything else that looks like a renderer is counted as unmapped, never turned into an item |
| ordering | document order within the page; the page order is upstream's and is not re-sorted |
| ids | only ids that pass `VideoId` validation become items; an invalid id is a skipped item with a reason |
| duplicates | reported in `duplicates_removed` and *dropped*, so a page never repeats a video id; the number is reported because "returned + duplicates" rows is a different fact from "fewer rows" |
| limit | `limit` bounds the items returned; a bound that truncates is reported (`page_truncated`) so a client never mistakes a shortened page for a complete one |
| next cursor | only from `continuationItemRenderer.continuationEndpoint.continuationCommand.token`; filter chips (`chipCloudChipRenderer`) and sort menus (`sortFilterSubMenuRenderer`) also carry `continuationCommand` tokens and are refused by `parse::find_page_cursor` |

A `next_cursor` is minted only when a genuine page token exists *and* it differs from the token that produced the
current page; a self-repeating token yields `next_cursor: None` plus a warning, so a client cannot be walked into an
unbounded continuation loop.

## 9. Verification status

| Claim | Status | Evidence |
| --- | --- | --- |
| one `browse` call returns the channel page, tabs and header counts | **live-verified** | probe run 43561555917: `id`, 7 tabs, `subscribers=21 400 000`, `videos=550`, no parse warnings |
| the videos tab needs the relayed `params` blob | **live-verified** | same run: relayed blob 20 bytes, tab response carried a page cursor of 136 bytes |
| current listing rows are `richItemRenderer > content > lockupViewModel` | **live-verified** | same run: the page's row shape was sampled and reported |
| lockup rows map to items (id, title, thumbnails, duration badge) | **fixture-verified** (synthetic, keyed like the sample) | `browse_channel_videos_lockup.json` |
| the tab strip order, slugs and `selected` flag | **fixture-verified** | `browse_channel_page.json` |
| ambiguous single-count rows are not guessed | **fixture-verified** | `browse_channel_page_ambiguous_counts.json` |
| a handle is not accepted upstream | **live-verified** | same run: `browseId: "@veritasium"` → `400` |
| cursor minting, tamper detection, expiry, fingerprints | **unit-tested** | `vydra-common` cursor tests, `vydra-engines` browse tests |
| links, country, join date, total views | **not implemented** | needs the `about` tab, out of scope for Phase 6 |

## 10. Cache policy

| Resource | TTL | Why |
| --- | --- | --- |
| channel document, `available` | `metadata_ttl_secs` (300 s) | titles, counts and badges move on the scale of minutes |
| channel document, `unavailable` | `unavailable_ttl_secs` (60 s) | private can become public; unavailability is reversible |
| channel document, `unknown` | never | the next request may get a verdict |
| video listing, page 1 | `listing_ttl_secs` (120 s) | uploads arrive, but not per second |
| video listing, continuation page | `cursor_page_ttl_secs` (60 s) | page 2 is only coherent shortly after page 1 |
| any error | never | a failure is `Err`; nothing is stored, so an outage is never remembered as unavailability |

Cache keys include the resource id, `hl`, `gl`, the page token and the limit it was read with, plus the configured
client fingerprint; a cursor carries the same dimensions in its signed payload, so a cursor minted under one
fingerprint is invalid under another.

## 11. Reproducing the checks

```bash
# unit + fixture tests (no egress)
cargo test -p vydra-providers --test channel_parser
cargo test -p vydra-domain channel
cargo test -p vydra-engines browse

# live probe (needs egress to www.youtube.com; do not run in the development sandbox)
VYDRA_PROBE_CHANNEL_ID=UCHnyfMqiRRG1u-2MsSQLbXA \
VYDRA_PROBE_CHANNEL_HANDLE=@veritasium \
cargo run --release -p vydra-providers --example probe_browse
```

The probe prints one `PROBE …` line per capture and republishes them as CI annotations
(`.github/workflows/probe.yml`), because the job log is not downloadable from this environment and the raw captures
are megabytes.
