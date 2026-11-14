# InnerTube request contracts — what Vydra sends and what it expects back

**Status of the evidence.** Vydra made its first live requests in Phase 4.3 (2026-11-14, runs
`43541388070` and `43542839419`; five captures analysed in
`docs/research/SEARCH_RESPONSE_ANALYSIS.md`). Rows are therefore labelled with three classes:

* **verified_live** — Vydra sent the request through the production transport and recorded the
  response it got back (run id + status + body size are cited);
* **recorded** — a *third party's* recording, reduced into this repository (see
  `docs/research/FIXTURE_PROVENANCE.md`);
* **inferred** — read out of Vydra's code and a public reference implementation, never observed.

Only `/youtubei/v1/search` has verified_live rows. The player endpoint, the `params` encoding and
everything behind the unverified-filters capability are still inferred, and are labelled that way.
Nothing below is a claim about what upstream *should* answer.

Phase 4.3 scope: evidence and reliability for the existing search provider. No new engine, no new
endpoint, no signature / `n-sig` / PoToken handling, no fabricated playback URL, no enabled filters.

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
| `context` object | `innertube/request.rs::ClientContext::new` | yt-dlp `_base.py:852-866` (`{'context': …, **query}`); **verified_live** (every capture carried it) |

Two divergences from MediaServiceCore are intentional and documented:

* MediaServiceCore also sets `alt=json`. Vydra does not; the default response is already JSON and
  adding an unverified parameter is not justified by any recording.
* MediaServiceCore copies `visitorData` into the header even when it is empty (empty header value);
  Vydra omits the header instead. An empty header is treated as "absent" upstream, so the difference
  is not observable.

### 3.1 Typed bodies (`innertube/request.rs`)

Since Phase 4.3 the bodies are structs, not `json!` literals, so the contract is enforced by the type
rather than by review:

| Type | Wire shape | Guarantee |
| --- | --- | --- |
| `ClientContext` | `{client{hl,gl,clientName,clientVersion,originalUrl[,platform][,visitorData]}, user{lockedSafetyMode}, request{useSsl,internalExperimentFlags}}` | one constructor for every endpoint; `visitorData` omitted when the bootstrap produced none; member order fixed |
| `SearchRequest` | `{context, query[, params]}` | `params` is private, `None` by default, and skipped when absent — a search without filters cannot accidentally serialize one |
| `ContinuationRequest` | `{context, continuation}` | **structurally cannot** carry `query` or `params` (no such fields) |
| `PlayerRequest` | `{context, videoId, contentCheckOk, racyCheckOk}` | the two flags are always present, as the web client sends them |

Field order is struct order, so the same request always serializes to the same bytes; the seven unit
tests in `request.rs` assert the exact string, including `"platform":"DESKTOP"` for the web profile
and the absence of `visitorData` when it is empty. `tests/request_contract.rs` keeps the endpoint,
query-string and header assertions on top.

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
| first page | `context`, `query` | **verified_live**: run `43542839419`, 200, 664 211 bytes; also `SearchApiHelper.java:6-8`, yt-dlp `_base.py:852-866` |
| first page + filters | `context`, `query`, `params` | **inferred**: `SearchApiHelper.java:27` — only used when the unverified-filters capability is enabled, which it is not |
| continuation | `context`, `continuation` | **verified_live**: run `43542839419`, 200, 263 144 bytes, 18 items on the second page; also `SearchApiHelper.CONTINUATION_SEARCH`, yt-dlp `_base.py:1056` |
| malformed body (no `query`) | `context` only | **verified_live**: 400, 274 bytes, `FAILED_PRECONDITION` — the error path is real, not imagined |

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
* **Verified_live** (Phase 4.3): a token taken from the first page was sent back in a body of exactly
  `{context, continuation}` and upstream answered 200 with a complete second page — 263 144 bytes, 18
  `videoRenderer` items and a further 670-char cursor (run `43544223832`). The envelope of that page
  is *not* the first page's: it arrives as
  `onResponseReceivedCommands[].reloadContinuationItemsCommand.continuationItems[]`
  (`docs/research/SEARCH_RESPONSE_ANALYSIS.md` §2).
* **History**: the first attempt (run `43542839419`) sent a *filter-chip* token instead of the page
  cursor — the extraction bug analysed in `docs/research/SEARCH_RESPONSE_ANALYSIS.md` §2.1, fixed in
  `parse.rs::find_continuation`. The capture above is from the run made after that fix, and its second
  page repeats page 1's total estimate, which is what a real continuation does.
* **Recorded** (Phase 4.2): a `/youtubei/v1/browse` continuation
  (`recorded/continuation_browse_2023-02-23.json`: 5 `richItemRenderer.content.videoRenderer` items +
  a trailing token, 582 chars, expired) — kept because it exercises `richItemRenderer` unwrapping,
  which search continuations do not use.
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

**Phase 5A: verified live.** Eight `POST /youtubei/v1/player` requests from two CI runs
(`43549141233`, `43549736295`) were accepted with HTTP 200 and `application/json; charset=UTF-8`. The
same video was sent with `true/true` and with `false/false`; the redacted responses were byte-identical,
so the flags did not influence the verdict for that case. Field-by-field contract, plus what is
deliberately not sent: `docs/research/PLAYER_REQUEST_CONTRACT.md`.

### Response (recorded, and one live shape)

The live captures returned **HTTP 200 with a refusal inside the body**: `UNPLAYABLE` with full metadata
for one public video, `LOGIN_REQUIRED` and `ERROR` with no metadata for the other two. The HTTP status
therefore carries no information about usability; see `docs/research/PLAYER_RESPONSE_ANALYSIS.md` §2–4.

Verified live (2026-11-14, run `43549736295`): `playabilityStatus.{status,reason,errorScreen}`,
`videoDetails.{videoId,title,author,channelId,lengthSeconds,viewCount,keywords,shortDescription,thumbnail}`,
`microformat.playerMicroformatRenderer.{publishDate,category,ownerChannelName,thumbnail}`.
**Not** observed live: `streamingData`, `captions`, `storyboards` (playback was refused on every
capture). Those rows below still rest on the third-party recording:

`recorded/player_web_2025-11-10.json` (MediaServiceCore, web client, 2025-11-10) carries:

| Path | Used for |
| --- | --- |
| `playabilityStatus.status`, `.reason` | `Availability` (`OK` → Available, otherwise mapped reason + warning) |
| `videoDetails.{videoId,title,lengthSeconds,viewCount,author,channelId,shortDescription,keywords,isLiveContent,isUpcoming,isLiveNow}` | `VideoSummary`, description, live state, chapters |
| `streamingData.formats[]`, `streamingData.adaptiveFormats[]` | `MediaFormatInfo` — **metadata only, never URLs** (ADR-006) |
| `captions.playerCaptionsTracklistRenderer.captionTracks[]` | `CaptionTrack` (`kind:"asr"` → Automatic, absent → Manual) |
| `microformat.playerMicroformatRenderer.{publishDate,category,thumbnail}` | `published_at`, `category`, thumbnails |

Guards (Phase 5A, ADR-020): the verdict is classified **before** `videoDetails` is required, because a
refusal legitimately arrives without metadata — that case becomes `ProviderError::Unavailable` (409), not
a parse error. With metadata present, a non-OK verdict yields a document whose `availability` names the
reason; an unrecognised verdict is never promoted to `Available`, and its raw status stays in the error
message and the warning list. A missing `playabilityStatus` yields `Availability::Unknown` plus a
**counted** warning (`missing_field:playabilityStatus.status`); strict mode turns it into `provider_parse`.
`videoDetails.videoId` is required and must equal the requested id, otherwise the document is a `Parse`
error (documented stale-client behaviour).

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
* Response body cap: `max_response_bytes` (default 4 MiB) → `Parse` error mentioning the cap; the
  body is streamed and the cap is checked before the next chunk is appended
  (`transport.rs::execute_once`).
* Redirects: none (`Policy::none()`); a `3xx` cannot move the request to another host.

---

## 6. What is still not verified (after Phase 4.3)

| Row | Status | Why it stays open |
| --- | --- | --- |
| `/search` container path | **verified_live** | two captures, both the first page and a continuation |
| search continuation envelope | **verified_live** | token-only body accepted; different envelope, now documented |
| `sw.js_data` bootstrap shape | **verified_live (indirectly)** | every run produced a visitor id and an API key and the subsequent request was accepted; the bootstrap document itself is not stored |
| `params` protobuf encoding | **inferred** | filters stay disabled (`enable_unverified_filters: false`); one filtered request/response pair would be needed, and it is not worth sending unverified bytes |
| player request + playability + metadata sections | **verified_live** (2026-11-14, runs `43549141233`, `43549736295`) | 8 requests, 8× HTTP 200; see `PLAYER_RESPONSE_ANALYSIS.md` |
| player `streamingData` / `captions` / `storyboards` | **recorded (2025-11-10) only** | every live capture was refused playback (`UNPLAYABLE`, `LOGIN_REQUIRED`, `ERROR`); closing this needs a non-datacenter egress, not a code change |
| region / language variance | **open** | one capture, `gl=US`, `hl=en` |
| Shorts / lockup cards | **counted, unmapped** | see `docs/research/SEARCH_RESPONSE_ANALYSIS.md` §3 |

What the remaining rows need is another live capture, not more reading. The capture path exists
(`crates/vydra-providers/examples/probe_search.rs` + `.github/workflows/probe.yml`) so a later phase
can extend it instead of guessing.
