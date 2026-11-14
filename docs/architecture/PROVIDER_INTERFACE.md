# Provider Interface Contract

This document is normative: implementations in `crates/vydra-providers` and consumers in
`crates/vydra-provider-registry` must satisfy it. It exists because no reference implementation in
`docs/research/` offers a provider abstraction (see `ARCHITECTURE_COMPARISON.md` §8).

---

## 1. Traits

```rust
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    fn id(&self) -> ProviderId;                     // stable, lowercase, e.g. "innertube-web"
    fn capabilities(&self) -> CapabilitySet;
    fn client_profile(&self) -> &ClientProfile;     // informs provenance + cache key
    fn health_snapshot(&self) -> ProviderHealth;    // cheap, lock-free read

    async fn fetch(
        &self,
        request: ProviderRequest,
        ctx: &ProviderContext,
    ) -> Result<ProviderResponse, ProviderError>;
}
```

`ProviderRequest` is a closed enum of typed resource requests (`Search`, `Video`, `Channel`,
`Playlist`, `Comments`, `CaptionTracks`, `Live`, `Trending`, `Related`, `Categories`). Adding a
resource is an API change; adding a provider is not.

`ProviderContext` carries: per-call deadline (`Instant`), request id, locale/country, an optional
`dyn Transport` override (tests), and the `CacheHandle` for session/bootstrap state.

### 1.1 Optional capability traits

Capabilities that need extra machinery are **separate traits**, so providers stay simple (this is
NewPipe's `PoTokenProvider` lesson, generalized):

```rust
pub trait SessionAware { fn invalidate_session(&self); }
pub trait TranscriptCapable { async fn transcript(..) -> ..; }
```

A provider that does not implement `TranscriptCapable` simply does not declare
`CAPTIONS_TRANSCRIPT`, and the registry will never route that request to it.

---

## 2. Capability set

Bitflags over `(resource, feature)`:

| Group | Flags (non-exhaustive) |
|---|---|
| Search | `SEARCH_ALL`, `SEARCH_VIDEOS`, `SEARCH_CHANNELS`, `SEARCH_PLAYLISTS`, `SEARCH_MUSIC`, `SEARCH_FILTERS`, `SEARCH_SORT`, `SEARCH_SUGGESTIONS` |
| Video | `VIDEO_METADATA`, `VIDEO_RELATED`, `VIDEO_CHAPTERS`, `VIDEO_FORMATS_METADATA`, `VIDEO_SUBTITLES_METADATA` |
| Channel | `CHANNEL_METADATA`, `CHANNEL_VIDEOS`, `CHANNEL_PLAYLISTS`, `CHANNEL_ABOUT`, `CHANNEL_TABS` |
| Playlist | `PLAYLIST_METADATA`, `PLAYLIST_ITEMS` |
| Comments | `COMMENTS_THREAD`, `COMMENTS_REPLIES`, `COMMENTS_SORT` |
| Captions | `CAPTIONS_METADATA`, `CAPTIONS_TRANSCRIPT` |
| Live | `LIVE_METADATA`, `LIVE_CHAT_METADATA` |
| Discovery | `TRENDING`, `CATEGORIES`, `RECOMMENDATIONS` |
| Cross-cutting | `CURSOR_PAGINATION` (provider can produce a next cursor), `LOCALE_SENSITIVE` (respects hl/gl) |

Rules:

- A provider must **only** declare what its tests cover. Declaring a capability without a passing
  fixture test is a review failure.
- Capabilities are reported by `GET /status` and used to decide which routes are mounted.
- `CapabilitySet` is `const`-constructible and `Copy`.

---

## 3. Results and errors

```rust
pub enum ProviderResponse {
    Search(Page<SearchItem>),
    Video(VideoDocument),
    Channel(ChannelDocument),
    Playlist(PlaylistDocument),
    Comments(Page<Comment>),
    Captions(CaptionTrackList),
    Live(LiveDocument),
    Trending(Page<VideoSummary>),
    Related(Page<VideoSummary>),
    Categories(Vec<Category>),
}
```

Every variant embeds `Provenance` (`provider, endpoint, client, fetched_at, cached,
parse_warnings`) and, where applicable, `next: Option<Cursor>`.

```rust
pub enum ProviderError {
    Unavailable { reason: UnavailabilityReason, message: String }, // content fact: no fallback
    NotFound { id: String, message: String },                      // content fact: no fallback
    Unsupported { capability: Capability },                        // registry bug: no fallback
    InvalidRequest { field: &'static str, issue: &'static str },    // no fallback
    RateLimited { retry_after: Option<Duration> },                  // fallback + breaker
    Unauthorized { message: String },                               // invalidate session, then fallback
    Upstream { status: u16, message: String },                       // fallback + breaker
    Timeout { elapsed: Duration },                                   // fallback + breaker
    Transport { message: String },                                   // fallback + breaker
    Parse { message: String, report: ParseReport },                  // fallback + alert
    CircuitOpen { until: Instant },                                  // never attempted
}
```

**Fallback policy** (`ProviderError::is_fallback_eligible`): `RateLimited | Unauthorized | Upstream |
Timeout | Transport | Parse` → yes; everything else → no. This is the single most important rule in
the contract: falling back on `Unavailable` wastes quota and hides content facts.

---

## 4. Health and circuit breaking

```rust
pub enum CircuitState { Closed, Open { until: Instant }, HalfOpen }
```

- Windowed statistics (default 30 requests / 60 s) per provider and per capability group.
- Opens on `error_rate > 0.5` with `n >= 10`, or immediately on 3 consecutive `RateLimited`.
- Half-open admits **one** probe request; success closes, failure re-opens with exponential cooldown
  (5 s → 10 s → … capped at 5 min) plus ±20 % jitter.
- `Unavailable`/`NotFound` never affect health.
- State is in-process (authoritative) and optionally exported to Redis for cross-replica visibility
  (read-only mirror; a replica never trusts a remote "open" state for longer than 30 s).

Health surfaces:

- `ProviderHealth { state, success_rate, p95_latency_ms, consecutive_failures, last_error_code }`
- Prometheus: `vydra_provider_circuit_state{provider,state}`, `vydra_provider_requests_total{provider,
  capability, outcome}`.
- `GET /status` exposes a sanitized snapshot (no upstream messages, no URLs).

---

## 5. Selection algorithm (normative)

```
select(request):
  cap = request.required_capability()
  candidates = providers.filter(p => p.capabilities().contains(cap))
                        .filter(p => p.health.state != Open)
                        .sort_by(priority desc, p95 latency asc)
  if candidates.is_empty():
      if any provider had the capability but all are Open -> 503 provider_circuit_open
      else -> 503 capability_unavailable
  attempts = min(config.max_attempts, candidates.len())     # default 2, hard cap 3
  for (i, p) in candidates[..attempts]:
      outcome = p.fetch(request)
      record(p, outcome)
      if ok: return with meta.sources += {provider: p.id, attempt: i, latency_ms}
      if !outcome.is_fallback_eligible(): return error (mapped)
  return last error
```

Determinism: given the same provider set, priority and health state, the same candidate order results.
No randomness in selection (jitter only affects cooldowns).

---

## 6. Bounded fallback across *different* data richness

Providers do not return identical data (requirement 6). Two rules keep normalization honest:

1. A successful response from a lower-priority provider must still satisfy the **domain minimum**
   for the requested resource (e.g. a video document requires id + title, or it is `Parse`).
2. Fields that a provider cannot supply are `None` and recorded in `meta.warnings` as
   `"field_unavailable: chapters"` — never filled from a different provider, never zeroed.

Cross-provider *enrichment* (calling provider B to fill a field missing from A) is **disabled by
default** and, when enabled, must be explicit per field, budgeted, and reported in `meta.sources`.

---

## 7. Provider implementation checklist

A new provider is complete only when:

- [ ] `id()` is stable and namespaced by transport (`innertube-web`, `html-search`, …).
- [ ] `capabilities()` matches the fixture tests that exist for it.
- [ ] Every `ProviderError` variant it can produce is covered by a `FailingTransport` test.
- [ ] Its requests go exclusively through `Transport` (no direct `reqwest` usage).
- [ ] Deadlines are respected: it must not run past `ctx.deadline`.
- [ ] Provenance is populated (provider/endpoint/client/fetched_at).
- [ ] Pagination either returns a real `Cursor` or declares no `CURSOR_PAGINATION`.
- [ ] No `unwrap`/`expect`/`panic!` in request paths; no `println!`.
- [ ] `ParseReport` is passed through, not swallowed.
