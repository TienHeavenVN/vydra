# Extraction Methods

Technical comparison of every extraction technique observed in the ecosystem, with the evidence
(file paths) and Vydra's decision for each. Read together with `IMPLEMENTATION_DECISIONS.md`, which
records the *decision*, while this file records the *evidence*.

---

## 1. The two families

| | HTML scraping | InnerTube |
|---|---|---|
| Transport | `GET /results?search_query=…`, `GET /watch?v=…` | `POST /youtubei/v1/{endpoint}` (+ `GET /youtubei/v1/{endpoint}` for some) |
| Payload format | HTML → embedded `var ytInitialData = {…}` / `ytInitialPlayerResponse = {…}` / `ytcfg.set({…})` | JSON body with a `context` object |
| Parser input | HTML (Cheerio, regex, brace matching) | JSON (Gson/JSONPath, serde, class-per-renderer) |
| Fields available | Exactly what the results page renders | Superset: unrendered metadata, all formats, all tabs |
| Pagination | Must be re-derived (`sp=`, page tokens) or `continuation` from the JS payload | Opaque `continuation` strings, stable contract |
| Localization | Depends on `Accept-Language`/consent cookies; numbers rendered localized | `context.client.hl` / `.gl` control language **and** numeric formatting |
| Failure modes | Layout change, lazy-loaded data, consent interstitial, bot challenge HTML | `playabilityStatus`, `error.code`, degraded client, wrong-result substitution |
| Cost per request | ~0.5–2 MB HTML | 20–400 KB JSON |
| Used by | `Unofficial-YoutubeApi` (only), Invidious/NewPipe/yt-dlp (fallback), MediaServiceCore (`InitialResponseService`, `ytcfg`) | everything else |

**Verdict:** InnerTube is the primary transport for every feature. HTML scraping is kept for exactly
two jobs: (1) *bootstrap* (`apiKey`, `visitorData`, `clientVersion`, `coldConfigData`), and
(2) *degraded-mode search* when InnerTube returns a hard failure.

---

## 2. Bootstrap chain

### 2.1 `sw.js_data` (MediaServiceCore, YouTube.js)

```
GET https://www.youtube.com/sw.js_data
→ [ [ { "deviceInfo": {...}, "visitorData": "..." } ], "AIzaSy…", … ]
```

- `MediaServiceCore/youtubeapi/src/main/java/com/liskovsoft/youtubeapi/innertube/core/Session.kt`
  (`SessionApi.getSessionData`) reads `deviceInfo` + `apiKey` from it.
- `YouTube.js/src/core/Session.ts` (`getSessionData`, `#getSessionData`) uses the same endpoint and
  falls back to `ytcfg` embedded in the HTML when it fails.

**Observed advantage:** one small request gives a *device-consistent* `visitorData` + api key pair,
which is what the config request and subsequent browse calls expect.

### 2.2 `/v1/config` (MediaServiceCore only)

`Session.retrieveInnertubeConfig` POSTs a synthetic context to `youtubei/v1/config` and extracts
`responseContext.globalConfigGroup.{rawColdConfigGroup.configData, coldHashData, hotHashData}`.

These strings are echoed back inside `context.client.configInfo` on later requests. Whether they
change extraction outcomes is **not documented by any project examined**; Vydra implements the field
as optional pass-through (`InnertubeContext.config_info: Option<ConfigInfo>`) and does not depend on
it. This is recorded as an assumption, not a verified requirement.

### 2.3 `ytcfg` from HTML (Invidious, YouTube.js, NewPipeExtractor, yt-dlp)

Regex/JSON extraction of `ytcfg.set({…})` provides `INNERTUBE_API_KEY`, `INNERTUBE_CLIENT_VERSION`,
`VISITOR_DATA`, `SESSION_INDEX`, `HL`, `GL`. Invidious keeps this path because Crystal has no
service-worker path; NewPipe uses it as the source of the *real* client version
(`YoutubeParsingHelper`, see the comment about "JavaScript service worker, then from HTML search
results page as a fallback").

### 2.4 Consent / cookies

- `NewPipeExtractor/.../YoutubeParsingHelper.java` builds a `CONSENT`/`SOCS` cookie set to avoid the
  EU consent interstitial.
- `invidious/src/invidious/yt_backend/youtube_api.cr` sets `x-youtube-bootstrap-logged-in` based on
  whether cookies are configured.

**Vydra:** bootstrap responses are cached with a TTL and tagged with the `visitorData` that produced
them; consent cookies are a configuration toggle (`YOUTUBE__CONSENT_COOKIE`), never hardcoded user
data, and never logged.

---

## 3. InnerTube request composition

### 3.1 URL and query

All projects agree on the essentials:

```
POST https://www.youtube.com/youtubei/v1/search?prettyPrint=false
```

(Numeric/`alt=json` variants exist: `MediaServiceCore` sets `prettyPrint=false&alt=json`; Invidious
and NewPipe set only `prettyPrint=false`.) Some projects use the api-key path
(`…/youtubei/v1/search?key=AIza…`), others rely on the `X-Youtube-Client-Name`/`-Version` headers.
Both work in the references; Vydra sends headers **and** the standard `prettyPrint=false`, and keeps
the api-key query parameter behind a config flag (observed differences mean neither is guaranteed to
be permanent).

### 3.2 Body

```json
{
  "context": {
    "client": {
      "hl": "en", "gl": "US",
      "clientName": "WEB", "clientVersion": "2.20260907.06.00",
      "originalUrl": "https://www.youtube.com", "platform": "DESKTOP",
      "utcOffsetMinutes": 0,
      "visitorData": "<opaque>"
    },
    "user": { "lockedSafetyMode": false },
    "request": { "useSsl": true, "internalExperimentFlags": [] }
  },
  "query": "…",           // endpoint-specific
  "params": "<base64url protobuf>",
  "continuation": "…"
}
```

- `NewPipeExtractor/.../YoutubeParsingHelper.java#prepareDesktopJsonBuilder` is the clearest reference
  for the `context` shape.
- `MediaServiceCore/.../innertube/core/HTTPClient.kt#processJsonPayload` shows per-client mutations of
  the body (clientName/clientVersion overwrite, `isWebKids` handling).
- `rustypipe/src/request_body.rs` (`ytbody!` macro) is the best Rust ergonomics reference for building
  these bodies while keeping keys optional.

**Vydra:** a typed `InnertubeContext` + per-endpoint `*Request` structs built with serde; unknown
fields allowed on deserialize, never on serialize (avoid sending garbage upstream).

### 3.3 Headers

| Header | Value seen | Source |
|---|---|---|
| `Content-Type` | `application/json` | all |
| `X-Youtube-Client-Name` | numeric proto id (`1` = WEB, `67` = WEB_REMIX, `101` = VISIONOS) | `ClientsConstants.java`, `youtube_api.cr` (`name_proto`) |
| `X-Youtube-Client-Version` | e.g. `2.20260907.06.00` | all |
| `User-Agent` | per-client; Android `com.google.android.youtube/<ver> (Linux; U; Android …)`, iOS `com.google.ios.youtube/…` | `Constants.kt`, `ClientsConstants.java` |
| `X-GOOG-API-FORMAT-VERSION` | `2` for Android/protobuf | `HTTPClient.kt` |
| `Accept-Encoding` | `gzip, deflate` (must decompress manually in Crystal; reqwest does it) | `youtube_api.cr#_decompress` |
| `Origin`/`Referer` | `https://www.youtube.com` | `Constants.kt` (`WEB.REFERER`) |
| `X-Goog-Visitor-Id` | visitor data, some clients | `yt-dlp/_base.py` |

---

## 4. Client matrix: why it exists and what it changes

Observed differences (verified by reading the constants and the branching code, not by measuring):

1. **Field availability.** `WEB` gives full search/browse metadata but media URLs from `WEB` player
   responses are the ones most affected by signature/throttling rules. `TVHTML5`/`WEB_EMBEDDED`
   responses historically omit some fields but return different playability outcomes.
   `NewPipeExtractor/.../InnertubeClientRequestInfo.java` encodes exactly this per-client feature
   matrix.
2. **Localization.** `WEB_REMIX` (Music) has a completely different renderer set.
3. **Auth.** `MediaServiceCore/HTTPClient.kt` notes YouTube Kids does not accept regular bearer
   tokens.
4. **Version drift.** Client versions are hardcoded with dated fallbacks; every project treats
   refreshing them as routine maintenance (`ClientsConstants.java` comments cite app-store pages;
   `Constants.kt` carries `WEB.VERSION = 2.20260907.06.00`).

**Vydra model:** `ClientKind` enum + `ClientProfile` data (`client_name`, `client_id`,
`client_version`, `user_agent`, `api_key`, `hl/gl` defaults, `capabilities: CapabilitySet`).
Profiles are configuration-overridable (`VYDRA__PROVIDER__CLIENTS__…` env) so an operator can rotate a
version without a release. `capabilities` drives provider *selection*, not request branching.

---

## 5. Search

### 5.1 Request

```json
{"context": {…}, "query": "rust", "params": "<optional filter token>"}
```

Filter token is a **protobuf varint bitfield**, not plain base64 text:

- `invidious/src/invidious/search/filters.cr` (374 LOC) builds it field-by-field.
- `rustypipe/src/param/search_filter.rs` + `src/util/protobuf.rs` produce the same layout
  (`Feature` enum values 4/5/6/7/8/14/15/25/26 are protobuf field numbers).
- `MediaServiceCore/.../search/SearchFilterHelper.java` does it for the TV client.

Vydra implements `params` encoding as a small protobuf writer
(`crates/vydra-providers/src/innertube/params.rs`) with round-trip tests against vectors
captured from the reference implementations' documented layouts — flagged in code as
`unverified_against_live_api` until live verification is possible.

### 5.2 Response shapes

First page:

```
contents.twoColumnSearchResultsRenderer.primaryContents.sectionListRenderer.contents[]
  → itemSectionRenderer.contents[] → videoRenderer | channelRenderer | playlistRenderer | …
  → continuationItemRenderer.continuationEndpoint.continuationCommand.token
```

Continuation page (`MediaServiceCore` calls it `WatchNextResultContinuation`):

```
onResponseReceivedCommands[] / continuationContents.{itemSectionRenderer|sectionListContinuation}
```

`rustypipe/src/client/search.rs`, `invidious/src/invidious/search/*.cr`, and
`YouTube.js/src/parser/classes/` all encode the same tree with different type names.

**Vydra:** one `parse_search_page(Value) -> Page<SearchItem>` that (a) walks a *list of known wrapper
paths* rather than one hardcoded path, (b) emits a `ParseReport` for unknown renderer keys instead of
failing, and (c) returns the union of every `continuationCommand.token` found as `next`.

### 5.3 Pagination contract

Nobody decodes continuation tokens; all projects round-trip them opaquely. Vydra wraps them in a
`Cursor` with an HMAC + provider tag so the public API never leaks raw provider state and cannot be
used as an amplification vector:

```
GET /api/v1/search/videos?q=rust&cursor=eyJwIjoiaW5uZXJ0dWJlIiwidiI6MSwiayI6Ii4uLiIsInMiOiJ4In0.signature
```

---

## 6. Video metadata and playability

### 6.1 Endpoints

- `POST /youtubei/v1/player` `{"videoId": "…", "context": {…}}` — canonical (`Session.kt`,
  `Actions.ts`, `rustypipe/src/client/player.rs`).
- `POST /youtubei/v1/next` `{"videoId": "…"}` — watch-next / related / comments continuation entry.
- HTML `GET /watch?v=…&bpctr=9999999999999&has_verified=1` → `ytInitialPlayerResponse`
  (`MediaServiceCore/.../initialresponse/InitialResponseService.kt`) — used as fallback and to avoid
  caching surprises.

### 6.2 Error signals

`playabilityStatus.status` values observed in the parsers:
`OK`, `ERROR`, `UNPLAYABLE`, `LOGIN_REQUIRED`, `CONTENT_CHECK_REQUIRED`, `AGE_CHECK_REQUIRED`,
`LIVE_STREAM_OFFLINE`, `AGE_VERIFICATION_REQUIRED`. Reasons are surfaced as `reason`/`subreason`
runs. `rustypipe/src/error.rs#UnavailabilityReason` enumerates the same set with the correct
distinction between "deleted", "private", "members-only", "age-restricted" and "geo-blocked".

**Vydra:** `Availability` enum in the domain model; never collapse into a generic 404, because
`LOGIN_REQUIRED` and `UNPLAYABLE` are operationally different (one may be a provider problem, the
other a content fact).

### 6.3 Formats

`streamingData.adaptiveFormats[]` and `.formats[]` with `itag`, `mimeType`, `codecs`, `bitrate`,
`width`/`height`, `fps`, `audioQuality`, `approxDurationMs`, `contentLength`, `qualityLabel`,
`audioTrack`, plus either `url` or `signatureCipher`/`cipher`.

- `NewPipeExtractor/.../ItagItem.java` is the best public mapping of itag → codec/DRM/container.
- `MediaServiceCore/.../impl/MediaFormatImpl.kt` shows how `signatureCipher` is normalized
  (`s`, `sp`, `url` triple).
- Live streams add `dashManifestUrl`/`hlsManifestUrl`; `MediaServiceCore` polls `DASH info`
  (`videoinfo/V2/DashInfoApi`) to get `availabilityStartTime`/segment durations for seeking.

### 6.4 The protected path (documented, not implemented)

Three mechanisms gate media URLs:

1. **`signatureCipher`** — `s` parameter must be transformed by the player JS function `decipher`.
2. **`n` parameter throttling** — a JS function transforms `n` in the URL; wrong/missing `n` yields
   throttled delivery (`YoutubeThrottlingParameterUtils.java`, `deobfuscate` in rustypipe).
3. **`pot` (Proof of Origin token)** — BotGuard-derived token appended as `pot=`; produced by running
   Google's BotGuard VM (`PoTokenGate`, `BgPoTokenProvider`, `yt_dlp/extractor/youtube/pot/`).

All three require executing or emulating Google's obfuscated JavaScript, i.e. circumventing an
integrity/anti-bot control. **Vydra does not implement them** (see ADR-006). Vydra exposes format
*metadata* (itag, codec, bitrate, size, whether a URL is `cipher`-protected or `pot`-required) and
clearly marks `playability: "restricted"` when the only obtainable URLs are protected.

---

## 7. Channels

Endpoints: `browse` with `{"browseId": "UC…"}` (landing/tabs), plus `{"browseId": "…", "params": "…"}`
for a specific tab, and `continuation` for further pages. `next` is not used for channels.

Reference implementations:
- `NewPipeExtractor/.../YoutubeChannelHelper.java` (561 LOC) — tab resolution, `params` for
  videos/shorts/live/playlists tabs, and the "channel about" continuation.
- `invidious/src/invidious/channels/*.cr` — channel + about + tabs.
- `rustypipe/src/client/channel.rs` — async flow incl. RSS-only fallback.

Tab `params` values are stable-in-practice base64 literals but are opaque; every project hardcodes
them. Vydra stores them in `crates/vydra-providers/src/innertube/params.rs` as named constants with a
comment recording origin, and treats an empty tab as "no data" rather than an error.

---

## 8. Playlists

`browse` with `{"browseId": "VL<playlistId>"}` (note the `VL` prefix applied by all projects —
`MediaServiceCore/.../playlist/`, `rustypipe/src/client/playlist.rs`). Continuations come from
`continuationItemRenderer`. Mixes/radio (`RD…`) and albums (`OLAK5uy_…`) use the same shape with
different renderers (`playlistVideoRenderer` vs `playlistPanelVideoRenderer`).

---

## 9. Comments

Two-step: `next` (or `browse`) yields a `continuationCommand.token` for the comment section; the
token is then posted as `{"continuation": "…"}`. Replies use a per-comment continuation token
obtained from `commentThreadRenderer.replies.commentRepliesRenderer.continuations[]`.

- `MediaServiceCore/.../comments/{CommentsApi.kt,CommentsApiParams.kt,gen/}`
- `YouTube.js/src/parser/classes/Comment*.ts`, `Comments*.ts`
- `invidious/src/invidious/comments/*.cr`

Sorting is expressed as a `params` token chosen from a fixed set (top/new). Vydra exposes
`sort=top|new` and maps to the observed tokens, marking it `experimental`.

---

## 10. Captions

Sources: `captions.playerCaptionsTracklistRenderer.captionTracks[]` → `baseUrl` (a `timedtext`
endpoint) with `languageCode`, `kind` (`asr` = auto-generated), `vssId`, `isTranslatable`, and
`translationLanguages[]`.

- `invidious/src/invidious/videos/caption.cr`, `videos/transcript.cr` (fetches `timedtext`, decodes
  `fmt=json3`/`srv3`/`vtt`).
- `MediaServiceCore/.../impl/MediaSubtitleImpl.kt` (metadata only).
- `yt-dlp` has the most complete subtitle handling (formats, translation, auto-translation).

**Vydra Phase 6:** caption *metadata* (tracks, languages, translatability) is served; transcript
fetching is implemented as a separate, explicitly-verified endpoint because `baseUrl` may require
the same request context (`pot`) as media URLs.

---

## 11. Live

- `playabilityStatus.liveStreamability`, `videoDetails.isLive`, `isLiveContent`, `isUpcoming`,
  `microformat.playerMicroformatRenderer.liveBroadcastDetails` (`startTimestamp`, `endTimestamp`).
- Live chat: `LiveChatApi.kt` + `LiveChatApiParams.kt` implement the InnerTube `live_chat` endpoints
  (`get_live_chat`, `live_chat/get_item_context_menu`) with continuation polling and a
  `params`-derived initial token fetched from the watch page's `ytInitialData`.

**Vydra:** live *metadata* only; live chat relays are out of scope for v1 (documented).

---

## 12. Normalization rules that must be shared, not duplicated

Every project re-implements these. Vydra centralizes them in `crates/vydra-common/src/text.rs`:

| Rule | Reference implementation | Known pitfalls |
|---|---|---|
| `"1.2M views"` → `1_200_000` | `lib/youtube.js#parseViewCount`, `yt_dlp/utils/_utils.py#parse_count` | `jt`/`rb`/`k`/`m`/`b` substring collisions; `"1.234"` thousands vs decimal |
| `"1:02:03"` → `3723s`, `"LIVE"` → `None` | `lib/youtube.js#parseDuration`, youtubei.js `timeToSeconds` | localized separators (`1.26.43`); live/premiere values |
| thumbnail pick | `lib/youtube.js#getBestThumbnail`, Invidious `helpers/utils.cr` | protocol-relative URLs; width ordering is *not* guaranteed sorted |
| `"2 days ago"` relative dates | only used for display by most; `rustypipe/src/util/timeago.rs` parses it | locale-dependent, lossy → keep raw text when no absolute date |
| published time | `microformat.publishDate` / `dateText` | absolute ISO vs relative text |

**Policy:** Vydra never invents a number. When only a localized string exists, it parses it and
records `precision: "approximate"` plus the raw text; when parsing fails, the field is `null` and a
parse warning is emitted. This is a direct consequence of review rule "never fabricate missing data".

---

## 13. Rate limiting and degradation signals

Observed across implementations:

| Signal | Meaning | Reference |
|---|---|---|
| HTTP 429 with `Retry-After` | hard rate limit | handled by `yt_dlp/networking/_helper.py`, Invidious pool |
| HTTP 403 on player | signature/poToken/IP reputation | `rustypipe`, `MediaServiceCore` (`isUnplayable` → `Session.invalidate()`) |
| `playabilityStatus: LOGIN_REQUIRED` | sign-in wall (bot suspicion) | NewPipe, yt-dlp |
| `error.code = FAILED_PRECONDITION` | bad `params`/continuation | Invidious `_post_json` error branch |
| Empty `contents` with 200 | silent degradation / wrong result | `rustypipe` `WrongResult`, `MediaServiceCore` "Empty search result" log |
| Response for a *different* video id | client too old | `rustypipe/src/error.rs#WrongResult` |

Vydra's health model (see `docs/architecture/PROVIDER_INTERFACE.md`) turns these into
`ProviderOutcome::{Ok, Empty, Degraded, RateLimited, Unauthorized, Upstream4xx, Upstream5xx, Timeout,
Transport}` so fallback and circuit breaking never rely on string matching.
