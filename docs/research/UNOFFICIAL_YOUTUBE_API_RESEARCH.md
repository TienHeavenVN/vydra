# Unofficial YouTube API — Multi-Repository Research

**Status:** Phase 1 research, completed 2026-11-13
**Method:** every repository below was cloned at `HEAD` and read at source level. Findings cite the
repository URL and the exact file path inside it. Nothing in this document is derived from blog
posts or README claims alone.

Repository snapshots used for this research:

| Repository | Ref | Language | License | ★ | Last push (as observed) |
|---|---|---|---|---|---|
| `yuliskov/MediaServiceCore` | `HEAD` | Java/Kotlin | MIT | 103 | 2026-11-12 |
| `FatihArridho/Unofficial-YoutubeApi` | `HEAD` | JavaScript | MIT | 17 | 2026-06-29 |
| `iv-org/invidious` | `HEAD` | Crystal | AGPL-3.0 | 25 976 | 2026-11-12 |
| `TeamPiped/Piped-Backend` | `HEAD` | Java | AGPL-3.0 | 171 | 2026-11-10 |
| `TeamNewPipe/NewPipeExtractor` | `HEAD` | Java | GPL-3.0 | 2 053 | 2026-11-11 |
| `yt-dlp/yt-dlp` | `HEAD` | Python | Unlicense | 207 433 | 2026-11-04 |
| `LuanRT/YouTube.js` (`youtubei.js`) | `HEAD` | TypeScript | MIT | 5 418 | 2026-11-11 |
| `TeamPiped/rustypipe` | `HEAD` | Rust | GPL-3.0 | 19 | 2026-11-09 |
| `AndromedaCLI/youtubei-rs` | `HEAD` | Rust | AGPL-3.0 | 25 | 2022-09-18 |
| `nick42d/youtui` | `HEAD` | Rust | MIT | 206 | 2026-11-07 |
| `tombulled/innertube` | `HEAD` | Python | MIT | 513 | 2025-07-01 |
| `sonorahq/ytmusic-rs` | `HEAD` | Rust | GPL-3.0 | 17 | 2026-11-06 |

Metadata was taken from the GitHub REST API on 2026-11-13.

---

## 1. Comparison table

| Project | Language | Architecture | Extraction method | Features | Limitations |
|---|---|---|---|---|---|
| **MediaServiceCore** | Java/Kotlin (Android library) | Retrofit interfaces + **JSONPath-annotated POJOs** + "gen" model packages; service-per-domain (`search/v1`, `search/v2`, `browse/v1`, `videoinfo/V1`, `V2`); session object holds apiKey, client config, player | InnerTube `POST /youtubei/v1/{player,browse,next,search,...}` as **TV/Android/WEB clients**; `ytInitialPlayerResponse` scrape as fallback/secondary path; Retromock fixtures for tests | Search v1+v2, video info + formats, channels, playlists, comments, live chat, captions, playback (signature + n-sig + poToken), auth (OAuth/device), SponsorBlock/DeArrow | Android-only deps (`android.net.Uri`, Retrofit, OkHttp, V8/QuickJS assets); Java 8; heavy per-client branching; needs JS engine for `nsig`/`sig` |
| **Unofficial-YoutubeApi** | JavaScript (Express + Vercel) | 3 files: `lib/youtube.js` (716 LOC total, ~628 parsing), `router/youtube.js`, `index.js` | **HTML scraping**: `GET /results?search_query=` → Cheerio → `var ytInitialData = {...}` extraction + balanced-brace recovery | Search (all/video/channel/playlist), localized duration/view-count parsing (id/en) | No pagination, no continuation, no video detail, no tests, no timeouts, no caching, view-count heuristics |
| **Invidious** | Crystal | Service-object `YoutubeAPI` with per-client `ClientConfig`; extractor modules (`yt_backend/extractors.cr`), route layer `routes/api/v1/*`, per-resource parsers under `videos/`, `search/`, `channels/`, `comments/` | InnerTube (`browse`, `next`, `player`, `search`, `get_transcript`) with **hardcoded multi-client matrix**; HTML parsing kept only for bootstrap values (`ytcfg`, `visitorData`); optional **companion service** for playback challenge solving | Extremely broad: search w/ filters, channels, playlists, comments, captions, trending, RSS, subscriptions, DASH/HLS manifest generation, proxy | AGPL-3.0 (viral); Crystal ecosystem/talent scarce; playback requires signature/poToken machinery via companion; DB (Postgres) tightly couples caching |
| **Piped-Backend** | Java (Servlets, `reqwest4j`) | Thin HTTP handlers (`server/handlers/*`) delegating to **NewPipeExtractor**; Hibernate+Liquibase+Postgres for user data; Redis optional | Delegated to NewPipeExtractor (InnerTube); own PoToken provider (`BgPoTokenProvider` calls `bgHelperUrl`) and `MpdBuilder` | Search, streams, channels, playlists, comments, trending, subscriptions, feeds, MPD/HLS manifest rewrite, SponsorBlock | AGPL-3.0; extractor logic not owned by the project (upstream breakage propagates); captcha solver / geo-bypass helpers are policy-sensitive; `reqwest4j` native glue |
| **NewPipeExtractor** | Java (Android-independent core) | **`Downloader` abstraction** (Request/Response objects) + `StreamingService` registry + `Extractor`/`InfoItemsCollector` generics; per-service packages | InnerTube with a **client-fallback ladder** (`ClientsConstants`, `YoutubeParsingHelper#getJsonPostResponse`), JS engine (Nashorn/GraalJS) for `nsig`, DASH manifest creators | Search, video/audio streams, channels, playlists, comments, kiosks, subscriptions, captions | GPL-3.0; mandatory JS engine for signature/throttling; sync/blocking API (no async); single-service-first design; complex inheritance |
| **yt-dlp** | Python | Massive extractor plugin system; YouTube split into `_base.py`, `_video.py`, `_tab.py`, `_search.py`, `_clip.py`, `_redirect.py`, `_notifications.py`, `_mistakes.py`; `jsc/` (JS challenge, pluggable providers) and `pot/` (PoToken providers) | InnerTube multi-client + HTML fallbacks; own `jsinterp.py` interpreter; format-selection algebra; retry/backoff per-request | Playback + metadata, everything format-related, live, subtitles, comments | Unlicense; Python runtime; monolithic `YoutubeDL`; built for download not API serving; no server layer |
| **YouTube.js** | TypeScript | `Innertube` class → `Session` (context/apiKey/config, sw.js_data caching) → `Actions`/managers → **class-per-renderer parser** (`parser/classes/*`, 449 classes) with generator-driven field validation | InnerTube only (no HTML parsing of result pages) | Full InnerTube surface incl. Music, live chat, studio, comments, continuations | MIT; 449 renderer classes = large maintenance surface; bitset-heavy validation; JS-ecosystem business |
| **rustypipe** | Rust | Async client with `CacheStorage` trait, `Report`/`Reporter` diagnostics, **tolerant deserializer (`MapResult`)**, protobuf param builders, `codegen` crate producing Rust types from captured test files | InnerTube (WEB / WEB_REMIX / TVHTML5 / Android…) + `rquickjs` for `nsig` deobfuscation + botguard PoToken | YouTube + YouTube Music: search, video, channel (incl. RSS), playlists, trends, artist/album/track | GPL-3.0; single-client design (library, not service); no HTTP API, no rate limiting, no persistence |
| **youtubei-rs** | Rust | `client/` (typed client per client-type), `api/endpoints.rs` + `api/query.rs`, serde models | InnerTube (browse/search/player…) | Basic search/browse/video | AGPL-3.0; **unmaintained since 2022**; hand-written models drifted from API |
| **youtui** | Rust | TUI + embedded local API server (MIT) | InnerTube via its own extractor layer | YT Music playback/search | MIT; music-focused; playback-centric |
| **innertube (Python)** | Python | Single-module client, extremely small (~1 file client + parsers) | InnerTube + protobuf-ish params | Search, video, channel, playlist, transcript | MIT; minimal error model; no channel pagination parity |
| **ytmusic-rs** | Rust | Rust port of `ytmusicapi` | InnerTube `WEB_REMIX` | YT Music only | GPL-3.0; music only |

### High-level verdict

1. **InnerTube (`POST /youtubei/v1/*`) is the only sustainable extraction base.** Every maintained
   project that needs more than search results uses it. HTML scraping of `ytInitialData` survives in
   two roles only: (a) bootstrapping (`ytcfg`/`visitorData`/API key), (b) as a degraded fallback for
   search. `Unofficial-YoutubeApi` is a pure scraper and is correspondingly the least capable.
2. **The fragile part of the ecosystem is not metadata — it is media URLs** (`signatureCipher`,
   `n`-parameter throttling, `pot` PoToken, SABR). Every project needs a JS engine or a sidecar for
   it. This is exactly the area Vydra must *model* but must not *circumvent* (see
   `IMPLEMENTATION_DECISIONS.md`, ADR-006).
3. **Data-model strategy splits into two camps**: (a) faithful renderer mirrors (YouTube.js: one
   class per renderer; MediaServiceCore: JSONPath-annotated "gen" models), and (b) normalized
   domain models (NewPipe: `StreamInfo`/`ChannelInfo`; rustypipe: `VideoItem`/`ChannelItem`).
   Camp (b) is the right base for a public REST API, camp (a) is the right base for *parsing*.
   Vydra uses (a) internally and (b) externally — see `ARCHITECTURE_COMPARISON.md`.
4. **Test strategy is the differentiator.** MediaServiceCore records HTTP fixtures (`retromock`,
   `test/resources/**/*.json`), rustypipe ships a 100+ MB `testfiles/` corpus with a codegen pass,
   yt-dlp has `test/test_youtube_*.py` + `extractor/youtube` sample tests. Piped/Invidious rely
   mostly on live smoke tests. Fixture corpora are the only way to get deterministic parser tests.

---

## 2. MediaServiceCore (required repo #1)

`https://github.com/yuliskov/MediaServiceCore` — MIT, 290 Java/Kotlin files, Gradle multi-module
(`youtubeapi`, `mediaserviceinterfaces`, `SharedModules`).

### 2.1 Layer structure

```
mediaserviceinterfaces/         pure interfaces (MediaItem, MediaGroup, MediaItemFormatInfo…)
youtubeapi/src/main/java/com/liskovsoft/
  googlecommon/common/converters/jsonpath/   @JsonPath annotation + Retrofit converter
  googlecommon/common/converters/regexp/     @RegExp converter (HTML/bootstrap scraping)
  youtubeapi/innertube/                      NEW consolidated client (core/, models/, ytcfg/, impl/)
  youtubeapi/search/{v1,v2}/                 two generations of search
  youtubeapi/browse/{v1,v2}/, next/{v1,v2}/
  youtubeapi/videoinfo/{V1,V2}/              playback info (+ DashInfoApi for live sync)
  youtubeapi/comments/, chat/, playlist/, channelgroups/
  youtubeapi/formatbuilders/{hlsbuilder,mpdbuilder,storyboard}/
  youtubeapi/app/                            AppService, PoTokenGate, ReloadPlaybackGate
```

Two generations coexist: `v1` (older Retrofit + JSONPath POJOs, per-endpoint APIs) and `v2`
(`<Domain>Service2` + `gen/` model packages + TV/Web clients). `InnertubeService.kt` is the newer
consolidation that supersedes `videoinfo/` for playback.

### 2.2 The JSONPath converter idea

`googlecommon/common/converters/jsonpath/JsonPath.java` declares `String[] value()` per field, and
`converter/JsonPathResponseBodyConverter.java` walks the path with `JsonPathTypeAdapter`. The result
is that a flat POJO can be populated from a deeply nested renderer tree without mirroring the tree:

```java
@JsonPath({"contents", "twoColumnSearchResultsRenderer", "primaryContents",
           "sectionListRenderer", "contents", "[0]", "itemSectionRenderer", "contents"})
List<SearchSection> sections;
```

This is a genuinely strong idea: it decouples the model from intermediate wrapper levels that
YouTube renames frequently. The cost is reflection-based mapping, no compile-time checking, and
silent nulls. Vydra keeps the *benefit* (typed, shallow models) with a different mechanism:
`serde_path_to_error`-style staged deserialization plus explicit "walk the wrapper chain" helper
functions (see `docs/architecture/DATA_FLOW.md`).

### 2.3 Session / client negotiation

`innertube/core/Session.kt` builds a session by:

1. `GET https://www.youtube.com/sw.js_data` → API key, device info, `visitorData`
   (`Session.getSessionDataResult` → `SessionApi.getSessionData`).
2. `POST https://www.youtube.com/youtubei/v1/config` with a `context` built from device info.
3. Reading `responseContext.globalConfigGroup.rawColdConfigGroup.configData`, `coldHashData`,
   `hotHashData` and putting them into `client.configInfo`.
4. Caching one session per `playerId` and invalidating on "unplayable".

`innertube/core/HTTPClient.kt` then rewrites headers per client: sets `X-Youtube-Client-Version`,
`X-Youtube-Client-Name`, `X-GOOG-API-FORMAT-VERSION: 2`, `User-Agent` for Android/iOS, and appends
`prettyPrint=false&alt=json`. It also processes the JSON body (`processJsonPayload`) before send.
`innertube/utils/Constants.kt` holds the literal client matrix: `IOS 21.26.4`, `VISIONOS 1.02`,
`WEB 2.20260907.06.00` + a public web API key (value redacted here on purpose: the reference projects hardcode it, Vydra does not ship it), `MWEB`, `TVHTML5`, etc.

**Lesson:** session bootstrap and header rewriting are first-class architecture, not glue. Vydra
models them as a `SessionProvider` with expiration, single-flight refresh and per-client caching
(see `IMPLEMENTATION_DECISIONS.md` ADR-004).

### 2.4 Search: the request shape

`search/SearchApiHelper.java` shows the minimal InnerTube request body:

```
FIRST_SEARCH          = {"query":"%s"}
FIRST_SEARCH_EXT      = {"query":"%s","params":"%s"}
CONTINUATION_SEARCH   = {"continuation":"%s"}
```

`PostDataHelper.createQueryTV(search)` wraps this in the TV client envelope. `SearchFilterHelper`
produces the `params` token (protobuf-encoded filter bitfield). Continuations are opaque strings
echoed back verbatim — no project decodes them, and neither will Vydra.

`search/v2/SearchService2.kt` is worth reading for two operational details:

- **Retry via `wrapper.clone()`** — a Retrofit `Call` is single-shot; the comment in the source
  documents a real bug where reuse raised `IllegalStateException("Already executed.")` instead of
  falling back. Lesson: retry objects must be constructible, not reused.
- **Two different results shapes** — first page returns `SearchResult` (section list), continuation
  returns `WatchNextResultContinuation`. Vydra normalizes both into one `Page<VideoSummary>`.

### 2.5 Playback: signature, n-sig, PoToken

`innertube/core/Player.kt`:

```kotlin
val result = appService.bulkSigExtract(formatInfo.clientInfo, extractNParams(urlHolders), extractSParams(urlHolders))
...
val poToken = PoTokenGate.getPoToken(formatInfo.clientInfo, formatInfo.videoId)
applySessionPoToken(urlHolders, poToken)
```

`extractNParams` is annotated "All throttled strings has same values" — i.e. one JS execution solves
all formats. The JS comes from the player script and is executed by native assets
(`youtubeapi/src/main/assets/nsigsolver`, `.../potokennp2/v8`) — a V8 build shipped per-ABI and a
JTNI wrapper. `PoTokenGate`/`BotGuard` generate `poToken` via BotGuard challenge.

`videoinfo/V2/DashInfoApi` is used to enable seeking on live streams (`formatInfo.sync(getDashInfo())`).

**Policy note for Vydra:** this is anti-bot / integrity measurement territory. Vydra documents and
models available formats but does not implement challenge solving (ADR-006).

### 2.6 Tests and fixtures

- Unit/instrumented tests: `youtubeapi/src/test/java/...` (`CommentsApiTest.kt`, `LiveChatApiTest.kt`,
  `innertube/impl/MediaItemFormatInfoImplTest.kt`, `formatbuilders/storyboard/YouTubeStoryParserTest.java`).
- Fixture-driven tests via **Retromock**: e.g. `browse/v2/mock/HomeApiMock.kt` declares
  `@MockResponse(body = "browse/tv/2025.04.06_home.json")`; fixtures live in
  `youtubeapi/src/test/resources/browse/{chrome,tv}/…` with dated filenames (e.g.
  `2023.02.23_channels(guide).json`).
- `androidTest/resources/potoken/` holds PoToken artefacts.

**Lesson adopted:** dated fixtures + a mock transport interface = deterministic parser tests.
Vydra's `vydra-providers` tests use exactly this shape (`Transport` trait + `tests/fixtures/*.json`).

### 2.7 Weaknesses to avoid

- Android coupling (`android.net.Uri`, `UriEncodedQueryString`) inside the HTTP client.
- Two parallel generations (v1/v2) with duplicated services and no deprecation path.
- Model drift: `videoinfo/models/*` mirrors renderer trees, so a rename breaks many files.
- No explicit timeout/backoff policy in the code read; retry logic is per-call ad hoc.

---

## 3. Unofficial-YoutubeApi (required repo #2)

`https://github.com/FatihArridho/Unofficial-YoutubeApi` — MIT, Express, 716 LOC total
(`lib/youtube.js` 628, `router/youtube.js` 61, `index.js` 16). Deployed as Vercel functions.

### 3.1 Request architecture

`router/youtube.js` exposes one route `/youtube/search` with four mutually exclusive query
parameters (`all`, `channel`, `video`, `playlist`); priority order documented in the README
("what the router will read first is the channel route"). Each branch calls a function in
`lib/youtube.js` which builds `https://www.youtube.com/results?search_query=…` (plus `&sp=` for
type filters) through Axios and hands the HTML to Cheerio.

### 3.2 Extraction

`extractYtInitialData(html)` (see `lib/youtube.js`):

1. `cheerio.load(html)`, iterate `<script>` tags.
2. Find `var ytInitialData = `, take the remainder, strip the trailing `;`.
3. `JSON.parse`; on failure, scan for balanced braces to recover a truncated payload.
4. Return `null` on total failure (the handler then returns an empty array).

Balanced-brace recovery is a real robustness trick worth keeping in a *fallback* parser. Vydra
implements it (`vydra-providers::innertube::extract_json_after`), with a hard size cap.

### 3.3 Normalization

The file's most portable contribution is **localized number/duration parsing**:

- `parseDuration` handles `H:MM:SS`, `MM:SS`, `SS`, and Indonesian `1.26.43` rendering.
- `parseViewCount` handles suffix multipliers in two locales (`jt`/`juta`, `rb`/`ribu`/`k`,
  `m`/`miliar`/`b`) with explicit word-boundary checks so that `"896.804 x ditonton"` does not
  accidentally match `b` in a neighbour word, and `parseNumberString` disambiguates `.`/`,` as
  thousands vs decimal separators by digit-group length.
- `getBestThumbnail` picks the last (largest) thumbnail and upgrades `//…` to `https://`.
- `isLiveVideo`/`isUpcomingVideo` read `thumbnailOverlays[].thumbnailOverlayTimeStatusRenderer.style`.

Vydra generalizes this into `vydra-common::text::{parse_view_count, parse_duration}` with unit tests
including the documented false-positive cases (locale list is data-driven, not branch-per-locale).

### 3.4 Limitations (all confirmed by reading the code)

- Pagination/continuation: **absent** (`lib/youtube.js` never sends `continuation`).
- Video/channel/playlist detail endpoints: **absent**.
- No timeouts, retries, caching, rate limiting, auth, tests, or schema validation.
- Errors are swallowed into `[]` — the caller cannot distinguish "no results" from "blocked".
- Relies on `ytInitialData` for `search` only; no `ytcfg` bootstrap, no consent/cookie handling.

**Verdict:** useful as a *catalogue of normalization heuristics* and as evidence that scraping-based
search is viable-but-brittle. Not an architecture reference.

---

## 4. Invidious

`https://github.com/iv-org/invidious` — AGPL-3.0, Crystal.

- `src/invidious/yt_backend/youtube_api.cr` (711 LOC): `ClientConfig` struct with a hardcoded
  client matrix (`HARDCODED_CLIENTS`, `ClientType::{Web, WebEmbedded, Android, Ios, Tvhtml5, …}`),
  `make_context` assembling the `context.client` object field-by-field, `_post_json` centralizing
  headers (`x-youtube-client-name` = *numeric* proto id, `x-youtube-bootstrap-logged-in`),
  gzip/deflate decompression (`_decompress`), and turning `{"error": {code, message}}` into a typed
  `InfoException`.
- `src/invidious/yt_backend/connection_pool.cr` — pooled HTTP clients with health checks; the
  precursor of the "circuit break" idea Vydra formalizes.
- `src/invidious/yt_backend/extractors.cr` (1292 LOC) — HTML/`ytInitialData` fallback parsers
  (`extract_items`, `extract_item`, `extract_videos`, `extract_channel_info`…).
- `src/invidious/search/filters.cr` (374 LOC) — search `params` builder; `search/query.cr`,
  `search/ctoken.cr` (continuation token helpers).
- `src/invidious/videos/parser.cr` (487 LOC) — `parse_video_info`, formats, captions; paired with
  `videos/formats.cr`, `videos/caption.cr`, `videos/storyboard.cr`.
- `src/invidious/routes/api/v1/*.cr` — the REST surface; `helpers/serialized_yt_data.cr` is the
  normalization layer that converts internal records to API JSON.
- `_post_invidious_companion` (in `youtube_api.cr`) delegates playback challenge handling to an
  **out-of-process companion**. Architecturally this is the cleanest handling of the
  signature/poToken problem: the anti-bot subsystem is isolated and replaceable.

**Lessons:** (a) centralize every InnerTube request in exactly one function; (b) keep a hardcoded
client matrix as *data*; (c) a separate serialization layer between internal records and the public
API is mandatory; (d) isolate anti-bot work in a separate process/service.

**License:** AGPL-3.0 — code cannot be reused in Vydra (we are not AGPL). Design ideas only.

---

## 5. Piped-Backend

`https://github.com/TeamPiped/Piped-Backend` — AGPL-3.0, Java servlets, 92 source files.

- `server/handlers/SearchHandlers.java` — validation-first handlers (`query` required, `StringUtils`),
  then straight delegation to NewPipeExtractor (`YOUTUBE_SERVICE.getSuggestionExtractor()`,
  `SearchInfo`) and `mapper.writeValueAsBytes(...)`. Domain objects in `utils/obj/*` are DTOs shaped
  for the Piped frontend.
- `utils/BgPoTokenProvider.java` implements NewPipe's `PoTokenProvider`, fetching `visitorData` by
  regexing the YouTube homepage HTML (`Pattern.compile("visitorData\":\"([\\w%-]+)\"")`) and calling
  an external `bgHelperUrl` to mint tokens. Confirms: PoToken mining is a *separate service*.
- `utils/MpdBuilder.java` rewrites YouTube's DASH manifest to proxy segments — the manifest is
  generated, not mirrored.
- `utils/CaptchaSolver.java`, `utils/GeoRestrictionBypassHelper.java` exist. Vydra will not port
  these: they are policy-sensitive and outside a metadata API's mandate.
- Test coverage: liberally relies on upstream NewPipeExtractor tests; no fixture corpus of its own.

**Lessons:** handler-level validation, explicit DTO layer, PoToken as external service. **Anti-lesson:**
inheriting an extractor it does not own means every upstream break is a production incident with no
local fixture to prove the fix.

---

## 6. NewPipeExtractor

`https://github.com/TeamNewPipe/NewPipeExtractor` — GPL-3.0, Java.

- `extractor/.../downloader/{Downloader,Request,Response}.java` — **the single most reusable design
  idea in the ecosystem.** Extraction code never touches an HTTP library; it depends on an abstract
  `Downloader` returning an immutable `Response` (code, message, headers, body, final URL). Tests
  inject a recorded downloader; production injects OkHttp. Vydra adopts this as `Transport`.
- `services/youtube/YoutubeParsingHelper.java` (1532 LOC) — `prepareDesktopJsonBuilder` composes the
  request body in a fluent JSON builder; `getJsonPostResponse(endpoint, body, localization)` appends
  `?prettyPrint=false` and sends via the downloader; client selection and fallback live here.
- `services/youtube/ClientsConstants.java` — client matrix with **numeric client ids**
  (`WEB_CLIENT_ID = "1"`, `WEB_REMIX_CLIENT_ID = "67"`, `VISIONOS_CLIENT_ID = "101"`) and
  `*_HARDCODED_CLIENT_VERSION` fallbacks with comments pointing at app-store pages for the current
  version. This is the maintenance ritual: refresh client versions when extraction degrades.
- `services/youtube/YoutubeJavaScriptPlayerManager.java` (346 LOC) + `YoutubeSignatureUtils.java` —
  signature/`n` solving through a JS engine; `YoutubeThrottlingParameterUtils.java` (251 LOC)
  handles `n` throttling parameters.
- `services/youtube/InnertubeClientRequestInfo.java` + `PoTokenProvider.java`/`PoTokenResult.java` —
  PoToken is an **optional interface** the embedding app may implement; must return `null` when it
  does not support a client. Excellent boundary: extraction works without it, degrades predictably.
- `ItagItem.java` (751 LOC) — the best public reference for itag → codec/bitrate/container metadata.
- `dashmanifestcreators/` — builds DASH manifests from adaptive formats.

**Lessons:** `Downloader` abstraction, PoToken-as-optional-capability, client matrix as data,
itag table as data. **Anti-lesson:** blocking (non-async) API, deep inheritance
(`StreamExtractor` → `YoutubeStreamExtractor`), and one giant parsing helper class.

---

## 7. yt-dlp

`https://github.com/yt-dlp/yt-dlp` — Unlicense.

- `yt_dlp/extractor/youtube/` is now split: `_base.py` (1371 LOC, client/visitor/consent
  bootstrap), `_video.py` (4599 LOC, player + formats), `_tab.py`, `_search.py`, `_clip.py`,
  `_redirect.py`, `_notifications.py`, `_mistakes.py`. Splitting a 6k-line extractor by *endpoint
  family* is a strong signal that per-endpoint modules beat one monolithic extractor.
- `yt_dlp/extractor/youtube/jsc/` — JS challenge solving with a **provider registry**
  (`provider.py`, `_registry.py`, `_director.py`, `_builtin/`), i.e. pluggable JS engines behind one
  interface.
- `yt_dlp/extractor/youtube/pot/` — PoToken provider registry (same pattern).
- `yt_dlp/jsinterp.py` — an in-house JavaScript interpreter, used when no external runtime is
  available.
- `yt_dlp/networking/` — a transport abstraction with per-request retry/backoff and impersonation
  support; `yt_dlp/cache.py` — a persistent, keyed cache used for player JS and metadata.
- `yt_dlp/_extractors.py` — generated registry listing all extractors (codegen for registration).

**Lessons:** (a) endpoint-family module split; (b) registry + trait for anything environment
dependent (JS engine, PoToken provider); (c) persistent cache for expensive derived artefacts;
(d) generated registries beat hand-maintained lists. Vydra mirrors (a), (b), (c) and uses a
`ProviderRegistry` with static construction instead of codegen.

---

## 8. YouTube.js (`youtubei.js`)

`https://github.com/LuanRT/YouTube.js` — MIT, TypeScript.

- `src/core/Session.ts` — bootstraps session data: `getSessionData` tries the service worker
  (`sw.js_data`) then falls back to `ytcfg` from the HTML, extracts `api_key`, `visitorData`,
  `clientVersion`, `config_data`; caches per options; supports `po_token`.
- `src/core/Actions.ts` — one method per InnerTube endpoint (`search`, `browse`, `next`, `player`,
  `getTranscript`, …), each constructing the endpoint-specific body.
- `src/parser/parser.ts` + `generator.ts` + `classes/` (449 classes) — parse renderer trees into
  typed class instances; the generator holds a per-class field schema (`PrimitiveInferenceType`,
  `RendererInferenceType`, `MiscInferenceType`, `ObjectInferenceType`) used to validate and to
  surface *parse warnings* rather than throwing.
- `src/core/clients/` — client definitions (WEB, ANDROID, TV, …) as data.

**Lessons:** schema-driven parsing with warnings; endpoint-per-method actions; client matrix as
data. **Anti-lesson:** 449 hand-maintained classes is a permanent tax; Vydra models only the
renderers it actually consumes and normalizes immediately.

---

## 9. Rust implementations

### 9.1 rustypipe (TeamPiped) — the closest prior art

`https://github.com/TeamPiped/rustypipe` — GPL-3.0, edition 2021, workspace
(`.`, `codegen`, `downloader`, `cli`).

- `src/error.rs` — `Error` + `ExtractionError` (`Unavailable { reason, msg }`, `NotFound { id, msg }`,
  `BadRequest`, `InvalidData`, `Deobfuscation`, `Botguard`, `WrongResult`, `Redirect`,
  `DeserializationWarnings`). This taxonomy is directly reusable as *design input*.
- `src/serializer/mod.rs`, `serializer/range.rs`, `serializer/vec_log_err.rs` — a tolerant mapping
  layer (`MapResult`) that collects warnings instead of failing on unknown/missing fields.
- `src/util/protobuf.rs` (`ProtoBuilder`) + `src/param/search_filter.rs` — protobuf varint
  encoding for search `params` (the same bit layout MediaServiceCore's `SearchFilterHelper` and
  Invidious' `search/filters.cr` produce).
- `src/request_body.rs` — `ytbody!` macro building request bodies declaratively
  (`?key: option, key: required, .. merge`).
- `src/cache.rs` — `CacheStorage` trait (`read`/`write`) + `FileStorage` JSON implementation; caches
  client versions and player JS.
- `src/report.rs` — diagnostic reporting (`Reporter`, `FileReporter`) recording extraction failures.
- `codegen/` — a CLI that downloads testfiles and generates Rust models/locale tables/dictionaries
  (`gen_dictionary`, `gen_locales`, `collect_*`).
- `tests/` + `testfiles/` (103 MB of captured responses) — the fixture corpus that makes the codegen
  and the snapshot tests possible.
- Client fallback: `src/client/mod.rs` holds the client list and per-client config; requests are
  retried with the next client when a response is degraded ("wrong result from YT").

**Divergences from Vydra:** rustypipe is a *library for a video frontend*, GPL-3.0 (no code reuse),
synchronous-ish ergonomics via its own `wreq` HTTP client, and it does deobfuscation/BotGuard work
that Vydra deliberately excludes.

### 9.2 youtubei-rs

`https://github.com/AndromedaCLI/youtubei-rs` — AGPL-3.0, 3885 LOC, **last push 2022-09-18**
(unmaintained). `src/client/client_types.rs`, `src/api/{endpoints,query}.rs`,
`src/models/{video,channel,playlist,enums,filter}.rs`. Shows the simplest workable Rust inner-tube
model layer; evidence that hand-written renderer mirrors decay within months.

### 9.3 youtui / ytmusic-rs

`nick42d/youtui` (MIT, 206★) — TUI with an embedded API server; `sonorahq/ytmusic-rs` (GPL-3.0) —
`WEB_REMIX`-only music client. Both confirm the client-matrix-as-data pattern and that *music*
endpoints are a separate client family, not a separate API.

### 9.4 innertube (Python, MIT)

`tombulled/innertube` — 513★, small and readable; good for cross-checking request bodies and the
shape of `next`/`player` responses; MIT so its *ideas* (not structure) are safe to consult.

---

## 10. Feature matrix observed across the ecosystem

Legend: ● implemented, ◐ partial/degraded, — absent.

| Capability | MSC | Unofficial-YT | Invidious | Piped | NewPipe | yt-dlp | YouTube.js | rustypipe |
|---|---|---|---|---|---|---|---|---|
| Search (videos) | ● | ● | ● | ● | ● | ● | ● | ● |
| Search (channels/playlists) | ● | ● | ● | ● | ● | ● | ● | ● |
| Search filters/sort (`params`) | ● | ◐ (`sp=`) | ● | ● | ● | ● | ● | ● |
| Suggestions | ● | — | ◐ | ● | ● | ◐ | ● | ● |
| Video metadata | ● | — | ● | ● | ● | ● | ● | ● |
| Chapters | ● | — | ● | ● | ● | ● | ● | ◐ |
| Captions/transcripts | ● | — | ● | ● | ● | ● | ● | ◐ |
| Comments + replies | ● | — | ● | ● | ● | ◐ | ● | — |
| Live chat | ● | — | ◐ | — | — | ◐ | ● | — |
| Channel about/stats | ● | — | ● | ● | ● | ● | ● | ● |
| Channel tabs + pagination | ● | — | ● | ● | ● | ● | ● | ● |
| Playlists (incl. mixes) | ● | — | ● | ● | ● | ● | ● | ● |
| Trending | ● | — | ● | ● | ● | ● | ● | ● |
| Related / next | ● | — | ● | ● | ● | ● | ● | ● |
| Media format URLs | ● (JS+poToken) | — | ● (companion) | ● (bgHelper) | ● (JS+poToken) | ● | ● | ● |
| Fixture-based parser tests | ● Retromock | — | ◐ | — | ◐ | ◐ | ◐ | ● corpus |
| Async/concurrent by design | — | ◐ | ● | ◐ | — | — | ● | ● |
| HTTP API surface | — (library) | ◐ | ● | ● | — | — | — | — (library) |

Nobody in this table separates *provider abstraction* from *extraction*, and nobody offers typed
capability discovery or bounded fallback as a first-class contract. That is Vydra's niche.

---

## 11. Sources of truth for future re-verification

| Topic | File to re-check when YouTube breaks something |
|---|---|
| Client matrix / versions | `NewPipeExtractor/.../youtube/ClientsConstants.java`, `MediaServiceCore/.../innertube/utils/Constants.kt`, `invidious/src/invidious/yt_backend/youtube_api.cr`, `YouTube.js/src/core/clients/` |
| Search filters (`params`) | `rustypipe/src/param/search_filter.rs`, `invidious/src/invidious/search/filters.cr`, `MediaServiceCore/.../search/SearchFilterHelper.java` |
| Session bootstrap | `MediaServiceCore/.../innertube/core/Session.kt`, `YouTube.js/src/core/Session.ts`, `yt-dlp/yt_dlp/extractor/youtube/_base.py` |
| Request bodies per endpoint | `YouTube.js/src/core/Actions.ts`, `NewPipeExtractor/.../YoutubeParsingHelper.java`, `rustypipe/src/client/*.rs` |
| Comment/continuation shapes | `MediaServiceCore/.../comments/gen/`, `YouTube.js/src/parser/classes/Comment*.ts` |
| Format metadata | `NewPipeExtractor/.../ItagItem.java`, `MediaServiceCore/.../innertube/impl/MediaFormatImpl.kt` |
| Renderer → domain mapping | `YouTube.js/src/parser/classes/`, `invidious/src/invidious/helpers/serialized_yt_data.cr` |
