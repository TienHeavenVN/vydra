# Source Code Map

A navigational index of the exact files read during Phase 1, per repository, with the reason each file
matters and what (if anything) Vydra adopted from it. Paths are relative to each repository root at
the snapshot listed in `UNOFFICIAL_YOUTUBE_API_RESEARCH.md`.

Use this file when YouTube changes a response shape: it tells you *where in the reference
implementations* to look first, and which Vydra file is the corresponding place to fix.

---

## 1. yuliskov/MediaServiceCore (MIT)

| Path | What it contains | Relevance to Vydra |
|---|---|---|
| `youtubeapi/src/main/java/com/liskovsoft/youtubeapi/innertube/InnertubeService.kt` | Two playback strategies side by side: `/player` InnerTube call ("LuanRT variation") and `ytInitialPlayerResponse` HTML scrape ("yt-dlp variation") | Confirms both player paths and their equivalence; basis for `ProviderKind::Player` vs `HtmlPlayer` fallback |
| `.../innertube/core/Session.kt` | `sw.js_data` bootstrap → `/v1/config` → cold/hot config data → cached session; `invalidate()` | Blueprint for `SessionProvider` (ADR-004) |
| `.../innertube/core/HTTPClient.kt` | Per-client header rewriting, `prettyPrint=false&alt=json`, `X-GOOG-API-FORMAT-VERSION: 2`, protobuf branch | Header rules in `vydra-providers/src/innertube/request.rs` |
| `.../innertube/core/Player.kt` | `decipher()`: bulk `n`/`sig` extraction, PoToken gating, DASH info for live seeking | Documents boundaries Vydra does **not** cross (ADR-006) |
| `.../innertube/utils/Constants.kt` | `URLS` (production/staging/testing youtubei hosts), `CLIENTS` matrix (IOS/VISIONOS/WEB/MWEB/TVHTML5…) | Client matrix format for `vydra-providers/src/innertube/clients.rs` |
| `.../innertube/utils/UserAgents.kt`, `Utils.kt`, `ApiHelpers.kt` | UA pool, JSON helpers, session/config header construction | UA selection policy |
| `.../innertube/ytcfg/{YtCfgApi,YtCfgService,YtCfgResult}.kt` | `ytcfg` extraction from HTML | Fallback bootstrap path |
| `.../innertube/initialresponse/InitialResponseService.kt` | `ytInitialPlayerResponse` regex extraction with `JSInterpret.searchJson` | HTML player fallback parser |
| `.../innertube/impl/{MediaFormatImpl,MediaSubtitleImpl,MediaItemFormatInfoImpl}.kt` | Format/subtitle model over player response | Field inventory for `MediaFormat` model |
| `<shared>/converters/jsonpath/JsonPath.java`, `converter/JsonPathResponseBodyConverter.java` | `@JsonPath(path)` annotation + Retrofit converter resolving deep paths | Idea of shallow models over deep JSON (reimplemented, not copied) |
| `.../search/SearchApiHelper.java` | `{"query":…}`, `{"query":…,"params":…}`, `{"continuation":…}` request bodies | Exact search body shapes |
| `.../search/SearchFilterHelper.java` | Filter bitfield → `params` token | Cross-check for protobuf param builder |
| `.../search/v2/SearchService2.kt` | First page (`SearchResult`) vs continuation (`WatchNextResultContinuation`); `wrapper.clone()` retry bug note | Page/continuation unification |
| `.../search/v2/gen/SearchResult.kt` | `data class SearchResult(val contents: Content?)` — thin top-level model | Confirms `contents` is the only root key that matters |
| `.../browse/v2/gen/` | Generated `Content`/`BrowseResult` models | Renderer normalization reference |
| `.../comments/{CommentsApi.kt,CommentsApiParams.kt,gen/}` | Comment continuation params and models | Comments engine design |
| `.../chat/{LiveChatApi.kt,LiveChatApiParams.kt,gen/}` | Live chat polling params | Live engine design |
| `.../formatbuilders/{mpdbuilder,hlsbuilder,storyboard}/` | DASH/HLS manifest builders, storyboard specs | Media Research Module |
| `youtubeapi/src/test/java/.../browse/v2/mock/HomeApiMock.kt` | Retromock fixture declarations with dated JSON names | Fixture-driven provider tests |
| `youtubeapi/src/test/resources/**/*.json` | Dated captured responses (`browse/chrome/…`, `browse/tv/…`) | What a real corpus looks like |
| `youtubeapi/build.gradle` | `retromock`, `robolectric`, `junit` test deps | Test tooling comparison |

## 2. FatihArridho/Unofficial-YoutubeApi (MIT)

| Path | What it contains | Relevance |
|---|---|---|
| `lib/youtube.js:1-60` | `parseDuration` incl. `1.26.43` locale form | `vydra-common::text::parse_clock_duration` cases |
| `lib/youtube.js:60-160` | `parseViewCount` + `parseNumberString` with word-boundary and separator heuristics | `parse_view_count` design + false-positive tests |
| `lib/youtube.js:~200-260` | `extractYtInitialData` with balanced-brace recovery | `extract_json_after` in the scraper fallback |
| `lib/youtube.js:~280-420` | `parseVideo`: title/owner/channelId/thumbnails/live badges from `videoRenderer` | Field precedence rules for `VideoSummary` |
| `router/youtube.js` | Single route, four query params, documented precedence | Reminder that public APIs must reject ambiguity |
| `vercel.json`, `index.js` | Serverless deployment wrapper | Not adopted (Vydra is a long-running service) |

## 3. iv-org/invidious (AGPL-3.0 — design reference only)

| Path | What it contains | Relevance |
|---|---|---|
| `src/invidious/yt_backend/youtube_api.cr` | `ClientConfig`, `HARDCODED_CLIENTS`, `make_context`, `browse`, `next`, `player`, `search`, `get_transcript`, `_post_json`, `_post_invidious_companion`, `_decompress` | Single choke-point request design; error mapping |
| `src/invidious/yt_backend/connection_pool.cr` | Pooled clients + health tracking | Circuit-breaker inspiration |
| `src/invidious/yt_backend/extractors.cr` | 1292 LOC of HTML/`ytInitialData` fallback parsers | Fallback parser catalogue |
| `src/invidious/yt_backend/url_sanitizer.cr`, `socks_proxy.cr` | URL hygiene, proxy support | SSRF hardening checklist |
| `src/invidious/search/{query,filters,ctoken,processors,hashtag}.cr` | Search query build, filter params, continuation tokens | Param encoding parity |
| `src/invidious/videos/{parser,formats,caption,storyboard,transcript,clip,music}.cr` | Player response parsing per concern | Separation of player concerns |
| `src/invidious/channels/*.cr`, `playlists/*.cr`, `comments/*.cr` | Per-domain parsing dirs | Module boundaries |
| `src/invidious/routes/api/v1/*.cr` | Public REST routes | REST surface comparison |
| `src/invidious/helpers/serialized_yt_data.cr` | Internal record → API JSON | DTO layer justification |
| `src/invidious/helpers/i18n.cr`, `tokens.cr` | Localization, token helpers | Not adopted |
| `spec/` | Crystal specs | Test comparison |

## 4. TeamPiped/Piped-Backend (AGPL-3.0 — design reference only)

| Path | What it contains | Relevance |
|---|---|---|
| `server/handlers/SearchHandlers.java` | Validation-first handlers delegating to NewPipeExtractor | Handler validation pattern |
| `server/handlers/{StreamHandlers,ChannelHandlers,PlaylistHandlers,TrendingHandlers}.java` | Per-domain handler sets | Endpoint grouping parity |
| `utils/BgPoTokenProvider.java` | `visitorData` regex + external token service | PoToken as external capability |
| `utils/MpdBuilder.java` | DASH manifest rewriting/proxying | Media module reference |
| `utils/RequestUtils.java`, `DatabaseHelper.java`, `DatabaseSessionFactory.java` | HTTP + Hibernate/Postgres persistence | Persistence comparison |
| `utils/CaptchaSolver.java`, `GeoRestrictionBypassHelper.java` | Explicitly out of scope for Vydra | Policy boundary |
| `utils/{ChannelHelpers,PlaylistHelpers,FeedHelpers,CollectionUtils}.java` | DTO mapping helpers | Normalization comparison |

## 5. TeamNewPipe/NewPipeExtractor (GPL-3.0 — design reference only)

| Path | What it contains | Relevance |
|---|---|---|
| `extractor/.../downloader/{Downloader,Request,Response}.java` | Abstract transport with immutable response | **Primary inspiration** for `vydra-providers::Transport` |
| `extractor/.../StreamingService.java`, `ServiceList.java` | Service registry + factory | Provider registry comparison |
| `extractor/.../InfoItemsCollector.java`, `MultiInfoItemsCollector.java` | Collectors converting extractors → models | Normalization pipeline comparison |
| `services/youtube/YoutubeParsingHelper.java` (1532) | Client selection, `prepareDesktopJsonBuilder`, `getJsonPostResponse`, URL/ID parsing | Client fallback ladder; body composition |
| `services/youtube/ClientsConstants.java` | Client ids/versions/UAs as constants | Client matrix |
| `services/youtube/InnertubeClientRequestInfo.java` | Which client supports which feature | Capability discovery! |
| `services/youtube/PoTokenProvider.java`, `PoTokenResult.java` | Optional PoToken capability interface | Capability as trait, not requirement |
| `services/youtube/YoutubeJavaScriptPlayerManager.java`, `YoutubeSignatureUtils.java`, `YoutubeThrottlingParameterUtils.java` | `nsig` solving + throttling params | Documented, not implemented (ADR-006) |
| `services/youtube/ItagItem.java` (751) | itag → codec/bitrate/container/drm table | Data source for `MediaFormat` |
| `services/youtube/dashmanifestcreators/*.java` | DASH manifest construction | Media module |
| `services/youtube/extractors/**` | Per-resource extractors (search/stream/channel/playlist/comments/kiosk) | Domain decomposition |

## 6. yt-dlp/yt-dlp (Unlicense)

| Path | What it contains | Relevance |
|---|---|---|
| `yt_dlp/extractor/youtube/_base.py` (1371) | Bootstrap, visitor data, client config, common helpers | Endpoint-family split reference |
| `yt_dlp/extractor/youtube/_video.py` (4599) | Player response, formats, subtitles, live | Format inventory |
| `yt_dlp/extractor/youtube/{_tab,_search,_clip,_redirect,_notifications,_mistakes}.py` | Per-concern extractors | Module boundary validation |
| `yt_dlp/extractor/youtube/pot/provider.py`, `_registry.py` | PoToken provider registry | Registry pattern |
| `yt_dlp/extractor/youtube/jsc/{provider,_registry,_director}.py`, `_builtin/` | JS engine provider registry | Registry pattern |
| `yt_dlp/jsinterp.py` | JS interpreter | Explicitly not adopted |
| `yt_dlp/networking/`, `yt_dlp/cache.py` | Transport abstraction, persistent cache | Cache keying / retry design |
| `yt_dlp/_extractors.py` | Generated extractor registry | Generated registries |
| `yt_dlp/utils/_utils.py` (`parse_count`, `parse_duration`) | Localized number/duration parsing | Cross-check of our parser rules |

## 7. LuanRT/YouTube.js (MIT)

| Path | What it contains | Relevance |
|---|---|---|
| `src/core/Session.ts` | `getSessionData` (sw.js_data → ytcfg fallback), api key, visitorData, config data, cache, po_token | Session bootstrap parity |
| `src/core/Actions.ts` | Endpoint-per-method InnerTube calls | Endpoint catalogue |
| `src/core/Player.ts` | Player response handling, deciphering | Boundary documentation |
| `src/core/clients/*` | Client definitions as data | Client matrix |
| `src/parser/parser.ts`, `generator.ts` | Schema-driven renderer parsing with warnings | Parse-warning model (ADR-005) |
| `src/parser/classes/*.ts` (449) | Typed renderer classes | Renderer inventory |
| `src/core/managers/*` | Feature managers (search, comments, playlist…) | Feature grouping comparison |
| `src/utils/*` | Helpers (`Constants`, `Utils`, `Cache`) | Cross-check |

## 8. TeamPiped/rustypipe (GPL-3.0 — design reference only)

| Path | What it contains | Relevance |
|---|---|---|
| `src/error.rs` | `Error`/`ExtractionError` taxonomy incl. `Unavailable`, `WrongResult`, `Redirect`, `DeserializationWarnings` | Error taxonomy input (rewritten) |
| `src/serializer/{mod,range,vec_log_err}.rs` | Tolerant mapping (`MapResult`) collecting warnings | Parse-warning model (ADR-005) |
| `src/request_body.rs` | `ytbody!` declarative body macro | Body builder ergonomics |
| `src/util/protobuf.rs`, `src/param/search_filter.rs` | Protobuf varint params, filter enums | `vydra-providers::innertube::params` |
| `src/util/visitor_data.rs`, `src/util/{date,timeago,dictionary}.rs` | Auxiliary normalization | `vydra-common::text` |
| `src/cache.rs` | `CacheStorage` trait + `FileStorage` | Cache abstraction |
| `src/report.rs` | Extraction diagnostics reporter | Observability of provider failures |
| `src/client/mod.rs` + `client/{search,video_details,channel,playlist,player,pagination}.rs` | Async client per domain; client fallback | Trait/registry shape |
| `src/deobfuscate*` (`rquickjs`) | `nsig` solving | Not implemented (ADR-006) |
| `codegen/src/*` | Model/locale/dictionary generation from testfiles | Codegen option (deferred) |
| `testfiles/`, `tests/` | 100+ MB fixture corpus, snapshots | Fixture strategy |

## 9. Rust others

| Path | What it contains | Relevance |
|---|---|---|
| `youtubei-rs/src/client/{client,client_types}.rs` | Typed client per client-type | Minimal Rust client shape |
| `youtubei-rs/src/api/{endpoints,query,extractors}.rs` | Endpoint + query builders | Simplicity baseline |
| `youtubei-rs/src/models/*.rs` | Hand-written renderer models (decayed) | Evidence against renderer mirrors |
| `ytmusic-rs/src/**` | `WEB_REMIX` client | Music client family |
| `youtui/src/**` | TUI + local HTTP API | Service-in-Rust prior art |

---

## 10. Mapping to Vydra

| Reference concept | Reference location | Vydra location |
|---|---|---|
| Transport abstraction | `NewPipeExtractor/.../downloader/Downloader.java` | `crates/vydra-providers/src/transport.rs` |
| Client matrix | `ClientsConstants.java`, `Constants.kt`, `youtube_api.cr`, `clients/*` | `crates/vydra-providers/src/innertube/clients.rs` |
| Session bootstrap | `Session.kt`, `Session.ts`, `_base.py` | `crates/vydra-providers/src/innertube/session.rs` |
| Request bodies | `SearchApiHelper.java`, `Actions.ts`, `request_body.rs` | `crates/vydra-providers/src/innertube/request.rs` |
| Filter params | `search_filter.rs`, `filters.cr`, `SearchFilterHelper.java` | `crates/vydra-providers/src/innertube/params.rs` |
| Renderer walking | `parser.ts`/`generator.ts`, `gen/` models, `extractors.cr` | `crates/vydra-providers/src/innertube/parse/*.rs` |
| Tolerant deserialization | `serializer/mod.rs` (`MapResult`) | `crates/vydra-providers/src/parse_report.rs` |
| Client fallback | `rustypipe/src/client/mod.rs`, `YoutubeParsingHelper` | `crates/vydra-provider-registry/src/fallback.rs` |
| Health/circuit breaking | `connection_pool.cr` | `crates/vydra-provider-registry/src/health.rs` |
| Cache abstraction | `cache.rs` (`CacheStorage`) | `crates/vydra-cache/src/lib.rs` |
| Localized text parsing | `lib/youtube.js`, `utils/_utils.py` | `crates/vydra-common/src/text.rs` |
| Fixtures | `test/resources/`, `testfiles/` | `crates/vydra-providers/tests/fixtures/` |
| Diagnostics | `report.rs` | `crates/vydra-observability/src/extraction_events.rs` |
