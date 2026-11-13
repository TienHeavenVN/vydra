# InnerTube request contracts — what Vydra sends and what it expects back

**Status of the evidence.** Vydra has never made a request to YouTube. Nothing in this document is a
live verification: every statement is either (a) read out of Vydra's own code, (b) read out of a
reference implementation that is publicly readable, or (c) checked against a *recorded* response that
a third-party test suite captured. Rows that only have (b) are marked **inferred**; rows that have a
recording are marked **recorded**. The distinction is deliberate — see
`docs/research/FIXTURE_PROVENANCE.md` and `docs/PROGRESS.md` for the standing rule.

Phase 4.2 scope: verify existing contracts. No new engine, no new endpoint, no signature /
`n-sig` / PoToken handling, and no fabricated playback URL.

---

## 1. Trace: public API → transport

```
HTTP GET /search
  └─ vydra-api        handlers::search                       (input validation, hl/gl normalization)
     └─ vydra-engines  Engine::search                        builds ProviderRequest::Search
        └─ vydra-provider-registry  Registry::fetch          capability + circuit + attempt budget
           └─ vydra-providers  InnertubeProvider::fetch     ProviderRequest → endpoint choice
              └─ InnertubeClient                             session, headers, body, error mapping
                 └─ Transport (ReqwestTransport)             URL validation, DNS guard, TLS, body cap
                    └─ https://www.youtube.com/youtubei/v1/{search|player}
```

| Hop | File | What it decides |
| --- | --- | --- |
| API | `crates/vydra-api/src/lib.rs` | query parsing, `hl`/`gl` normalization, error envelope |
| Engine | `crates/vydra-engines/src/lib.rs:167-177` | builds `ProviderRequest::Search`, cache key, attempts |
| Registry | `crates/vydra-provider-registry/src/lib.rs:504-575` | provider order, `max_attempts` (hard cap 3), health, `meta.sources` |
| Provider | `crates/vydra-providers/src/innertube/provider.rs:218-250` | `Search`/`Video`/`CaptionTracks` → `/search` or `/player` |
| Client | `crates/vydra-providers/src/innertube/client.rs` | bootstrap, headers, context, body, status→error |
| Transport | `crates/vydra-providers/src/transport.rs` | scheme/host/port/header/body validation, resolver guard, timeout |

`CaptionTracks` has no endpoint of its own: it calls the **player** endpoint and returns the caption
metadata from that response (`provider.rs:244-249`).

---

## 2. Session bootstrap

| Property | Value |
| --- | --- |
| Request | `GET https://www.youtube.com/sw.js_data` |
| Fallback | `GET https://www.youtube.com` (watch/home HTML) if the first one fails |
| Parsed fields | `visitorData`, `clientVersion`, api key (`AIza…`) via `find_field` / `find_json_string_field` |
| Cache | `SessionData` in memory, `session_ttl` (default 600 s), single-flight refresh |
| Invalidation | on `401`/`403` from any endpoint (`post_json`) |

Reference evidence: YouTube.js fetches `sw.js_data` and reads `visitor_data` + `api_key` from it
(`https://github.com/LuanRT/YouTube.js/blob/main/src/core/Session.ts#L419-L459`), and MediaServiceCore
keeps the same three values in its `Session` (`https://github.com/yuliskov/MediaServiceCore/blob/master/youtubeapi/src/main/java/com/liskovsoft/youtubeapi/innertube/core/Session.kt#L33-L62`).
**Inferred**: that `sw.js_data` currently still has the `[[{…}], "AIza…"]` shape Vydra parses. The two
paths are covered by unit tests (`client.rs` tests) using synthetic documents, not by a recording.

Documented upstream shape used by the tests:

```json
[[{"visitorData":"VISITOR"}],"AIzaSYFIXTUREKEY0123456789"]
```

---

## 3. Common envelope (every POST)

```
POST https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false&key={api_key}
content-type: application/json
x-youtube-client-name: 1
x-youtube-client-version: 2.20260805.01.00
origin: https://www.youtube.com
referer: https://www.youtube.com/
accept-language: {hl},en;q=0.8
user-agent: <WEB profile UA, Chrome 141 on Windows>
x-goog-visitor-id: {visitorData}          (omitted when the bootstrap produced none)
cookie: CONSENT=…                          (only when configured)
```

Body root (both endpoints):

```json
{"context":{
  "client":{"hl":"{hl}","gl":"{gl}","clientName":"WEB","clientVersion":"{version}",
            "originalUrl":"https://www.youtube.com","visitorData":"{visitor}"},
  "user":{"lockedSafetyMode":false},
  "request":{"useSsl":true,"internalExperimentFlags":[]}
}}
```

| Field | Source in Vydra | Reference evidence |
| --- | --- | --- |
| `prettyPrint=false` | `client.rs::endpoint_url` | MediaServiceCore `HTTPClient.kt:44`; yt-dlp `_base.py:862-866` |
| `key=` query parameter | `client.rs::endpoint_url` (**A-29 fix**, see below) | yt-dlp `_base.py:858-866`; MediaServiceCore `Session.kt:39` |
| `x-youtube-client-name` | `client.rs::headers` | yt-dlp `_base.py:978-1000` (`generate_api_headers`); MediaServiceCore `HTTPClient.kt:122-147` |
| `x-youtube-client-version` | same | same |
| `x-goog-visitor-id` | same | yt-dlp `_base.py:978-1000`; YouTube.js `src/utils/HTTPClient.ts#L192` |
| `context` object | `client.rs::context_value` | yt-dlp `_base.py:852-866` (`{'context': …, **query}`) |

Two divergences from MediaServiceCore are intentional and documented:

* MediaServiceCore also sets `alt=json`. Vydra does not; the default response is already JSON and
  adding an unverified parameter is not justified by any recording.
* MediaServiceCore copies `visitorData` into the header even when it is empty (empty header value);
  Vydra omits the header instead. An empty header is treated as "absent" upstream, so the difference
  is not observable.

### A-29 — the api key was never sent (fixed in this phase)

Before this phase, `post_json` built `…?prettyPrint=false` and stopped there: the key that the
bootstrap had just downloaded was stored in `SessionData.api_key` and never used. Every request was
keyless. `endpoint_url` now appends `&key=…`, the value is validated with `looks_like_api_key` before
interpolation (so a hostile bootstrap response cannot inject query parameters), it is never placed in
a header, and the key is never logged.

---

## 4. Search

### Request

```
POST /youtubei/v1/search?prettyPrint=false&key=…     (body: common envelope + the fields below)
```

| Case | Body keys | Evidence |
| --- | --- | --- |
| first page | `context`, `query` | `SearchApiHelper.java:6-8` (`{"query": …}`), yt-dlp `_base.py:852-866` |
| first page + filters | `context`, `query`, `params` | `SearchApiHelper.java:27` (`{"query":…,"params":…}`) — **only used when the unverified-filters capability is enabled** |
| continuation | `context`, `continuation` | `SearchApiHelper.CONTINUATION_SEARCH`, yt-dlp `_base.py:1056` |

The query is sent as a JSON string (no URL encoding needed); `hl`/`gl` live in the context, not in the
URL. Continuation does **not** repeat the query or the params — all three reference clients rely on
the token alone.

### Response (shape Vydra parses)

```
contents.twoColumnSearchResultsRenderer.primaryContents.sectionListRenderer
  .contents[].itemSectionRenderer.contents[]
     ├── videoRenderer      → SearchItem::Video
     ├── channelRenderer    → SearchItem::Channel
     ├── playlistRenderer   → SearchItem::Playlist
     └── continuationItemRenderer.continuationEndpoint.continuationCommand.token → next cursor
estimatedResults (number)  → estimated_results (Count::Exact)
```

Mapping lives in `crates/vydra-providers/src/innertube/map.rs` (`map_search`,
`search_item_from_renderer`); traversal in `crates/vydra-providers/src/parse.rs::collect_renderers`
(pre-order, document order — A-24) with `STRUCTURAL_RENDERERS` excluded from "unknown" counts (A-25).

**Verification status: inferred, not recorded.** No usable recording of a `/youtubei/v1/search`
response exists in the sources examined. The recorded corpus contains a *browse* document
(`twoColumnBrowseResultsRenderer.tabs[0].expandableTabRenderer.content.sectionListRenderer.contents[].itemSectionRenderer.contents[]`,
a channel's search tab) whose leaf renderers are the same `videoRenderer`/`playlistRenderer` shapes
Vydra maps. That recording therefore verifies the **leaf mapping** and the localized-value handling,
not the global search container path. `tests/fixtures/recorded/browse_channel_search_2023-12-06.meta.json`
records the gap in `not_verified`.

### Continuation

* Token extraction: recursive search for `continuationItemRenderer.continuationEndpoint.continuationCommand.token`
  (`parse.rs::find_continuation`), independent of container path.
* Cursor is opaque to Vydra; the engine encodes it for the public API and the provider sends it back
  verbatim.
* **Recorded**: a `/youtubei/v1/browse` continuation (`recorded/continuation_browse_2023-02-23.json`:
  5 `richItemRenderer.content.videoRenderer` items + a trailing token, 582 chars, expired). This
  verifies token extraction and `richItemRenderer` unwrapping; it does **not** prove that a search
  continuation has the same envelope.
* Page size: `limit` is applied locally after mapping and recorded as `page_truncated`, so Vydra
  never claims upstream honoured a page size it was not asked for.

---

## 5. Player

### Request

```
POST /youtubei/v1/player?prettyPrint=false&key=…
{"context":{…},"videoId":"9Umm2MAKsMc","contentCheckOk":true,"racyCheckOk":true}
```

`contentCheckOk`/`racyCheckOk` are the flags the web client sends so that age-restricted *metadata* is
not filtered out of the response; they are request-side only.

### Response (recorded)

`recorded/player_web_2025-11-10.json` (MediaServiceCore, web client, 2025-11-10) carries:

| Path | Used for |
| --- | --- |
| `playabilityStatus.status`, `.reason` | `Availability` (`OK` → Available, otherwise mapped reason + warning) |
| `videoDetails.{videoId,title,lengthSeconds,viewCount,author,channelId,shortDescription,keywords,isLiveContent,isUpcoming,isLiveNow}` | `VideoSummary`, description, live state, chapters |
| `streamingData.formats[]`, `streamingData.adaptiveFormats[]` | `MediaFormatInfo` — **metadata only, never URLs** (ADR-006) |
| `captions.playerCaptionsTracklistRenderer.captionTracks[]` | `CaptionTrack` (`kind:"asr"` → Automatic, absent → Manual) |
| `microformat.playerMicroformatRenderer.{publishDate,category,thumbnail}` | `published_at`, `category`, thumbnails |

Guards: `videoDetails` is required; a response whose `videoId` differs from the requested id is a
`Parse` error (documented stale-client behaviour); a missing `playabilityStatus` is a **counted**
warning (`missing_field:playabilityStatus.status`) rather than an invented reason.

Delivery classification (`map.rs::media_format_from`) never copies a URL: `drmFamilies` → `Drm`,
`url` present → `Direct`, `signatureCipher`/`cipher` → `SignatureProtected`, else `Unknown`. Nothing
attempts to decipher, unwrap, or construct a URL.

### Error handling (both endpoints)

| Status | `ProviderError` | Side effect |
| --- | --- | --- |
| `401`, `403` | `Unauthorized` | session invalidated (re-bootstrapped on the next call) |
| `404` | `NotFound` | — |
| `429` | `RateLimited` | `retry_after` propagated when present |
| other `4xx`/`5xx` | `Upstream { status }` | first 200 bytes of the body kept in the message |
| invalid JSON | `Parse` (`provider_parse`) | — |

### Timeouts, cancellation, size caps

* Per-request budget: `min(config.request_timeout, ctx.remaining())`; an expired context fails before
  the call is attempted (`ProviderError::Timeout`).
* Transport-level: `connect_timeout` 3 s; the whole exchange is bounded by the per-request timeout
  passed into `TransportRequest.timeout`.
* Response body cap: `max_response_bytes` (default 8 MiB) → `Parse` error mentioning the cap.
* Redirects: none (`Policy::none()`); a `3xx` cannot move the request to another host.

---

## 6. What would turn each "inferred" row into "recorded"

| Row | Missing evidence |
| --- | --- |
| `/search` container path | one recorded `/youtubei/v1/search` response (first page) |
| search continuation envelope | one recorded search continuation response |
| `sw.js_data` bootstrap shape today | one recorded bootstrap document, or a real request |
| `params` protobuf encoding | a recorded filtered-search request/response pair |
| player response *today* | the newest recording is 2025-11-10 and predates this phase by a year |

None of these can be produced without making a live request, which this phase deliberately does not
do. They are listed here, in `docs/PROGRESS.md` and in the phase report as the honest boundary.
