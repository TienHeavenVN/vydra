# Architecture Comparison

Comparison of *how* the reference implementations are structured (not what they can extract), and
which structure Vydra adopts. Every claim is traceable to a file listed in `SOURCE_CODE_MAP.md`.

---

## 1. Layering and dependency direction

| Project | Layering | Dependency direction | Where extraction logic lives |
|---|---|---|---|
| MediaServiceCore | `mediaserviceinterfaces` (pure) ← `youtubeapi` (impl) ← app | Impl depends on interfaces; interfaces know nothing about YouTube | Retrofit API interfaces + JSONPath POJOs + `*Service` orchestrators |
| Unofficial-YoutubeApi | router → lib | Direct | one 628-line module |
| Invidious | routes → domain modules → `yt_backend` | Bidirectional in practice (`videos/parser.cr` imports `yt_backend`) | `yt_backend` + per-domain parsers |
| Piped-Backend | handlers → DTOs → NewPipeExtractor | Downward, but the middle layer is an external library | **outside the repo** |
| NewPipeExtractor | `Downloader` ← extractors ← services registry | Clean top-down; extractors never see HTTP | `services/youtube/**` |
| yt-dlp | `YoutubeDL` core ← `InfoExtractor` ← concrete extractors | Clean; transport + cache injected | `extractor/youtube/*` |
| YouTube.js | `Innertube` → `Session`/`Actions`/`Parser` | Clean; parser independent of transport | `src/parser/**` |
| rustypipe | `client/*` → `model`/`serializer`/`param` → transport | Clean; model layer has no HTTP knowledge | `client/*` + `model/convert.rs` |
| youtubei-rs | `client` → `api` → `models` | Clean but unmaintained | `api/extractors.rs` |

**Synthesis.** The implementations that age well (NewPipeExtractor, yt-dlp, YouTube.js, rustypipe)
share three structural invariants:

1. **Transport is an injected abstraction**, never a hard dependency of extraction code
   (`Downloader.java`; yt-dlp's `networking`; YouTube.js' `fetch` option; rustypipe's cache/HTTP
   injection).
2. **Parsing is separate from model mapping** — raw response → typed renderer/parse tree → domain
   model. YouTube.js (`parser` → `classes`), rustypipe (`model/convert.rs`), NewPipe
   (`*Extractor` → `InfoItemsCollector`) all do this.
3. **The public surface is a DTO layer, not the parse tree** — `serialized_yt_data.cr` (Invidious),
   `utils/obj/*` (Piped), `MediaItem`/`MediaGroup` (MediaServiceCore).

The implementations that age badly (youtubei-rs) skipped (2) and (3): their serde models *are* the
renderer tree, so a single API rename breaks the public type.

---

## 2. Parser strategy

| Strategy | Example | Strengths | Weaknesses |
|---|---|---|---|
| **Renderer mirror** (one type per renderer) | YouTube.js `parser/classes/*` (449), MediaServiceCore `gen/`, youtubei-rs `models/` | Complete, predictable, easy to add a field | Huge surface; renames ripple; dead types accumulate (youtubei-rs decayed in months) |
| **JSONPath projection** | MediaServiceCore `@JsonPath` converter | Models stay shallow; wrapper renames don't break models | Reflection; silent nulls; no compile-time checks; debugging is hard |
| **Selector/collector walk** | NewPipe `InfoItemsCollector`, Invidious `extractors.cr#extract_item` | Small code, tolerant of extra keys | Type-unsafe; ad-hoc; duplicated per resource |
| **Tolerant mapping with warnings** | rustypipe `MapResult`, YouTube.js generator validation | Surfaces degradation instead of throwing; testable | Requires discipline; warnings must be actionable |

**Vydra:** strategy 1 *scoped to consumed renderers only* (a few `serde` structs per endpoint, with
`#[serde(default)]` everywhere) + strategy 4 (a `ParseReport` that counts unknown renderer keys and
missing required fields) and strategy 3's tolerance (never fail a whole page because one item is
malformed). Critically, the parse structs live in `vydra-providers` and are **never** part of the
public API types, which live in `vydra-domain`.

---

## 3. Error handling and degradation

| Project | Error model | Degradation behavior |
|---|---|---|
| MediaServiceCore | nullable returns + `Log.e`; `isUnplayable()` → `Session.invalidate()` | returns `null`, caller cannot distinguish causes |
| Unofficial-YoutubeApi | `try/catch` → `[]` | indistinguishable empty result |
| Invidious | `InfoException` with HTML-ish messages; `_post_json` maps `{"error":{code,message}}` | raises to route layer → HTTP error page/JSON |
| Piped | `ExceptionHandler.throwErrorResponse` + Sentry | maps to error JSON |
| NewPipeExtractor | checked `ExtractionException` subclasses (`ContentNotAvailableException`, `AgeRestrictedContentException`, …) | typed; caller decides |
| yt-dlp | `ExtractorError` with `expected` flag + retry at transport | retry/ignore/downloader decides |
| YouTube.js | `InnertubeError` + parse warnings array | tolerant parse, warnings surfaced |
| rustypipe | `Error` + `ExtractionError` enum incl. `WrongResult`, `DeserializationWarnings` | retries with next client, reports |

**Synthesis.** The two-pass separation matters more than the enum itself: *extraction* errors
(content unavailable) must be distinguishable from *transport* errors (timeout, 429, 5xx), because
only the latter justify fallback and retry. MediaServiceCore and Unofficial-YoutubeApi conflate them
and lose the ability to fail over correctly.

**Vydra:** three-level error model —

```
UpstreamOutcome (transport/provider layer, drives retry/fallback)
  → ProviderError (why this provider could not serve: Unavailable/Auth/RateLimited/Degraded/Parse)
    → ApiError (public HTTP contract: code + status + message + request id)
```

A `ContentUnavailable` provider error is **not** retried and does **not** trigger fallback by default
(it is a fact about content); `RateLimited`/`Timeout`/`Upstream5xx`/`Parse` do.

---

## 4. Caching

| Project | Cache layers |
|---|---|
| MediaServiceCore | in-memory session cache (`Session.cached`), `MediaServiceData` cookie store, OkHttp cache not used |
| Invidious | Postgres tables (`videos`, `channels`, `session_ids`), plus in-memory `PROXY_LIST`; jobs refresh entries |
| Piped | Postgres (user data) + optional Redis (`RequestUtils`), no metadata caching layer |
| NewPipeExtractor | none (library; caller's job) |
| yt-dlp | `yt_dlp/cache.py` (player JS, nsig cache, keyed JSON files) |
| YouTube.js | `ICache` interface; stores session data + player JS |
| rustypipe | `CacheStorage` trait + `FileStorage` JSON (client versions, deobf data) |

**Synthesis.** Three distinct cache classes are conflated by most projects:

1. **Session/bootstrap state** (apiKey, visitorData, client versions) — long TTL, single-flight,
   must survive restarts.
2. **Expensive derived artefacts** (player JS, solver output) — versioned by `player_id`.
3. **Response cache** (search pages, video metadata) — short TTL, per-provider, must be
   *stale-tolerant* (serve stale while revalidating) and *never* cached for error outcomes.

**Vydra:** `vydra-cache` defines a `Cache` trait with `get/set/invalidate` + TTL, an in-process LRU
implementation and a Redis implementation; keys are namespaced `vydra:v1:<class>:<provider>:<hash>`,
values carry `created_at`, `ttl`, `provider_version`, `parse_warnings`. Session state uses a separate
`SessionCache` with single-flight refresh. No response cache is enabled by default for
`playability`-affecting endpoints.

---

## 5. Concurrency model

| Project | Concurrency |
|---|---|
| MediaServiceCore | OkHttp thread pools; Retrofit blocking calls wrapped by the app |
| Invidious | Crystal fibers + `YT_POOL` connection pool with health checks |
| Piped | Servlet threads; `Multithreading.java` helper for fan-out |
| NewPipeExtractor | blocking, single-threaded API |
| yt-dlp | threads (`--concurrent-fragments`), not designed for server fan-out |
| YouTube.js | async/await, no concurrency limit |
| rustypipe | Tokio, `Arc<RwLock<…>>` for shared state, per-client semaphores |

**Synthesis.** Only Invidious and rustypipe treat concurrency limiting as a first-class concern.
For a public API the relevant limits are: (a) global in-flight requests, (b) per-provider in-flight
requests, (c) per-API-key request rate, (d) per-provider rate. Projects that rely on a thread pool
(Invidious' fixed pool) get implicit bounding; Tokio does not, so Vydra must implement it explicitly.

**Vydra:** `tower` layers for (c) and (d) at the edge, plus a per-provider `Semaphore` inside the
registry for (a)/(b), with `tokio::time::timeout` around every provider call and a global
`TimeoutLayer` at the HTTP edge.

---

## 6. Testing

| Project | Unit | Fixtures | Live/e2e | Determinism |
|---|---|---|---|---|
| MediaServiceCore | ● | ● Retromock JSON corpus, dated | ● (`*ApiTest` hitting YouTube) | fixture tests deterministic, live tests are not CI-gated |
| Unofficial-YoutubeApi | — | — | — | n/a |
| Invidious | ◐ | ◐ `spec/` mostly DB/route level | ● | low |
| Piped-Backend | ◐ | — | ● | low |
| NewPipeExtractor | ● | ◐ (`MockDownloader` in tests) | ● | mixed |
| yt-dlp | ● | ◐ (`test/test_*.py`, some samples) | ● (nightly) | mixed |
| YouTube.js | ● | ◐ | ◐ | mixed |
| rustypipe | ● | ●● 100+ MB `testfiles/` corpus + snapshots + codegen | ◐ | high |

**Synthesis.** The only way to keep a scraper honest is a **fixture corpus with dated captures and
snapshot-style assertions**, plus a clear separation between offline tests (CI-gating) and live tests
(informational). rustypipe is the reference; MediaServiceCore's dated fixtures are the second best.

**Vydra:**
- `crates/vydra-providers/tests/fixtures/**` — captured/synthetic payloads, one directory per
  endpoint family, each file with a header field recording provenance (`captured_live` vs
  `synthetic_schema_reference`) and capture date.
- `Transport` is a trait; tests use `ReplayTransport` (fixtures) and `FailingTransport` (error paths).
- Live tests are `#[ignore]`-gated and never counted as passing evidence.

---

## 7. Configuration

| Project | Configuration |
|---|---|
| MediaServiceCore | `AppClient` enums + hardcoded constants; no runtime config of client matrix |
| Invidious | YAML config (`config/config.example.yml`) + `CONFIG` singleton |
| Piped | env vars + `Constants.java` statics |
| NewPipeExtractor | constructor args (`Localization`, `ContentCountry`) |
| yt-dlp | CLI options object (`YoutubeDL` params) |
| YouTube.js | `Innertube.create({...})` options object |
| rustypipe | builder-style client config, feature flags |

**Synthesis.** Client/filter constants that *will* drift belong in configuration-overridable data,
not in `static final` fields. rustypipe's use of features + builder is the best Rust ergonomics.

**Vydra:** layered config (`defaults → file → env`), `vydra-config` with `serde` structs, every
provider/client knob overridable (`VYDRA__PROVIDER__INNERTUBE__CLIENTS__WEB__VERSION=…`), secrets only
via env/secret mount, and config validation at startup with a typed `ConfigError`.

---

## 8. Where the ecosystem leaves a gap (Vydra's contribution)

1. **No provider abstraction with capability negotiation.** Invidious/Piped/NewPipe are
   single-provider. Nothing can answer "which provider can serve *chapters* for this video, in
   French, with a 300 ms budget?" Vydra's `Provider` trait + `CapabilitySet` + registry does.
2. **No bounded fallback.** Fallback in the ecosystem is per-client inside one extractor
   (`WrongResult` → next client), not across independent providers, and is unbounded/ad hoc.
3. **No public error contract that preserves degradation.** Everything collapses to a generic error.
4. **No isolation between extraction and anti-bot machinery** except Invidious' companion process.
5. **No measured performance story.** Only rustypipe mentions async; nobody publishes limits,
   concurrency bounds or benchmark methodology for a *service*.
6. **No license-clean, MIT/Apache service implementation.** The best references (Invidious, Piped,
   NewPipeExtractor, rustypipe, ytmusic-rs) are all copyleft; MIT references (MediaServiceCore,
   YouTube.js, innertube, youtui) are Android-bound, TypeScript-bound, or music-only.

Vydra is therefore a clean-room reimplementation, taking *design lessons* (and only design lessons)
from copyleft projects, and *code-level ideas* only from permissively licensed ones.
