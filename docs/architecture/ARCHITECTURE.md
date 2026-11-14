# Vydra Architecture

**Phase 2 design. Status of implementation per module is tracked in `docs/PROGRESS.md`.**

Vydra is a modular monolith: one binary that can serve the API and one binary that runs maintenance
work, sharing a set of libraries. Service extraction (separate deployments) is a *documented option*,
not a starting point (ADR-001).

---

## 1. Crate graph

```
                         ┌─────────────────┐
                         │  apps/api-server│
                         └────────┬────────┘
                                  │
             ┌────────────────────┼────────────────────┐
             │                    │                    │
     ┌───────▼──────┐   ┌─────────▼───────┐   ┌────────▼────────┐
     │  vydra-api   │   │vydra-observability│  │   vydra-auth    │
     │ (axum router)│   └─────────┬───────┘   └────────┬────────┘
     └───┬──────┬───┘             │                    │
         │      │                 │                    │
  ┌──────▼──┐ ┌─▼────────────┐    │            ┌───────▼──────┐
  │vydra-   │ │vydra-engines │────┘            │  vydra-cache │
  │provider-│ │ search/video/│─────────────────┴──────────────┘
  │registry │ │ channel/...  │
  └──┬──────┘ └───┬──────┬───┘
     │            │      │
┌────▼────────────▼──┐ ┌─▼──────────────┐  ┌────────────────┐
│  vydra-providers   │ │ vydra-database │  │  vydra-config  │
│ (innertube, html)  │ └───────┬────────┘  └───────┬────────┘
└─────┬──────────────┘         │                   │
      │                        │                   │
┌─────▼──────────┐   ┌─────────▼──────┐   ┌────────▼────────┐
│ vydra-domain   │   │  migrations/   │   │  vydra-common   │
└─────┬──────────┘   └────────────────┘   └────────┬────────┘
      │                                            │
      └──────────────► vydra-errors ◄──────────────┘
```

Rules enforced by review (and by the absence of the reverse dependencies in `Cargo.toml`):

- `vydra-domain` depends on nothing but `serde`, `time`, `vydra-common`, `vydra-errors`.
  **No HTTP, no provider types.**
- `vydra-providers` may depend on `vydra-domain` (to produce it) but **not** on `vydra-engines`,
  `vydra-api`, or `vydra-database`.
- `vydra-engines` orchestrates providers + cache + database; it never depends on `axum`.
- `vydra-api` is the only crate that knows HTTP status codes, headers, and JSON envelopes.
- `vydra-provider-registry` sits *above* `vydra-providers` (it needs the trait, not the impls) and is
  consumed by `vydra-engines`.

## 2. Workspace layout

```
vydra/
├── apps/
│   ├── api-server/          # binary: axum server, graceful shutdown
│   └── worker/              # binary: cache warming, quota reset, health probes
├── crates/
│   ├── vydra-common/        # ids, text parsing, time, pagination primitives, bytes utils
│   ├── vydra-errors/        # error taxonomy + public error contract
│   ├── vydra-config/        # layered config (defaults → file → env), validation
│   ├── vydra-domain/        # normalized models, provenance, availability, capabilities
│   ├── vydra-providers/     # Transport trait, Innertube client, parsers, HTML fallback provider
│   ├── vydra-provider-registry/  # selection, health, circuit breaker, bounded fallback
│   ├── vydra-engines/       # search/video/channel/playlist/comments/captions/live/discovery
│   ├── vydra-auth/          # API keys, quota, rate limiting
│   ├── vydra-cache/         # Cache trait, in-memory LRU, Redis impl (feature), cursor codec
│   ├── vydra-database/      # sqlx pool, repositories, migrations runner
│   ├── vydra-observability/ # tracing/OTel/metrics setup, redaction, extraction events
│   └── vydra-api/           # router, handlers, extractors, middleware, OpenAPI
├── migrations/              # sqlx migrations (Postgres)
├── docs/{architecture,api,providers,research,security,operations}/
├── tests/                   # cross-crate integration tests (in-memory app)
├── benchmarks/              # criterion benches
├── deployments/{docker,kubernetes}/
└── .github/workflows/
```

ADR-001 explains why the domain engines are modules of `vydra-engines` rather than seven crates.

## 3. Request lifecycle

```
HTTP request
  │
  ├─ tower-http: TraceLayer (request id, span) / TimeoutLayer / BodyLimitLayer
  ├─ CORS (configurable, off by default)
  ├─ auth layer (vydra-auth): API key → ApiKeyContext{key_id, quota, scopes}
  ├─ rate-limit layer: per-key + per-IP token bucket (Redis or in-process)
  ├─ axum extractors: Path/Query/Json (typed, validated; invalid → 400 invalid_request)
  ▼
handler (vydra-api::handlers::*)
  │  builds an engine request struct (borrowed where possible)
  ▼
engine (vydra-engines::search::SearchEngine)
  │  1. cache lookup (class = search|metadata, key = normalized query hash)
  │  2. registry.select(capability, request) → ordered provider candidates
  │  3. for each candidate (≤ max_attempts):
  │       provider.fetch(req) → Result<ProviderResponse, ProviderError>
  │       registry.record(provider, outcome)   // health + circuit breaker
  │  4. normalize → domain model, attach Provenance + ParseReport counts
  │  5. cache store (never on error), emit metrics/events
  ▼
response: ApiResponse<T> envelope (data, meta{pagination, sources, warnings}, request_id)
```

## 4. Data flow invariants

1. **Provider → domain only.** A provider returns domain types or a typed `ProviderError`. No raw
   `serde_json::Value` escapes `vydra-providers`.
2. **Provenance always attached.** `source.provider`, `source.endpoint`, `source.client`,
   `source.fetched_at`, `source.cached`, plus `parse_warnings` count (ADR-016).
3. **Availability is content, not error.** `UNPLAYABLE`/`LOGIN_REQUIRED` produce a 200 document with
   `availability` populated and formats omitted (ADR-006).
4. **Cursors are opaque and signed** (ADR-008). The engine never persists them.
5. **Errors are typed at each boundary** and mapped exactly once, in `vydra-api::error`.

## 5. Module map (engines)

| Engine | Responsibility | Providers required | Public endpoints |
|---|---|---|---|
| `search` | query normalization, filters, pagination, mixed/sectioned results | `SEARCH_*` | `/search`, `/search/{videos,channels,playlists,music}` |
| `video` | metadata, related, chapters | `VIDEO_*, VIDEO_RELATED, VIDEO_CHAPTERS` | `/videos/{id}`, `/videos/{id}/{metadata,related,chapters}` |
| `channel` | metadata, tabs, about | `CHANNEL_*, CHANNEL_TABS, CHANNEL_ABOUT` | `/channels/{id}`, `/channels/{id}/{videos,playlists,about}` |
| `playlist` | metadata + items + ordering | `PLAYLIST_*` | `/playlists/{id}`, `/playlists/{id}/items` |
| `comments` | thread, replies, sorting | `COMMENTS_*, COMMENTS_REPLIES` | `/videos/{id}/comments`, `/comments/{id}/replies` |
| `captions` | track metadata, (verified) transcript | `CAPTIONS_METADATA`, `CAPTIONS_TRANSCRIPT` | `/videos/{id}/captions` |
| `live` | live metadata, live chat metadata | `LIVE_METADATA` | `/live`, `/live/{id}` |
| `discovery` | trending, categories, recommendations | `TRENDING_*, CATEGORIES, RECOMMENDATIONS` | `/trending`, `/categories`, `/recommendations` |

An endpoint is mounted **only if** at least one registered provider declares the required capability
at startup; otherwise it is omitted and listed as unimplemented in `GET /status`. This satisfies
requirement 7 ("only expose endpoints when the underlying functionality is implemented and tested").

## 6. Failure and degradation model

| Layer | Failure | Behavior |
|---|---|---|
| Transport | timeout / connect / TLS | retry once with backoff+jitter if idempotent; then `ProviderError::Transport` |
| Provider | 429 | honor `Retry-After`, mark provider `RateLimited`, circuit half-open timer |
| Provider | 5xx | count toward breaker; fallback to next provider |
| Provider | 401/403 on bootstrap | invalidate session, refresh once, then fail |
| Parser | unknown renderer keys | `ParseReport` warning, continue |
| Parser | structurally invalid body | `ProviderError::Parse`, fallback allowed, fixture regression alert |
| Engine | no provider can serve | `ApiError::capability_unavailable` (503) with `retryable=false` unless all were transient |
| Edge | body too large / invalid input | 413 / 400, never reaches providers |

## 7. Deployment shape

- One stateless `api-server` deployment (N replicas) + optional `worker` (1 replica, Postgres lease).
- Required: Postgres (durable: api keys, quotas snapshot, worker leases, optional metadata docs).
- Recommended: Redis (cache, quota counters, provider health replication).
- Health: `/health` (liveness, no dependencies), `/ready` (checks Postgres; Redis optional-degraded),
  `/status` (provider health, capabilities, build info), `/metrics` (Prometheus).
- No sticky sessions; cursors are self-contained.
