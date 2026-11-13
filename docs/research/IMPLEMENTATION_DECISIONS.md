# Implementation Decisions (ADR log)

Every decision below cites the research that motivated it (`docs/research/*.md`). Decisions are
written as ADRs so that later phases can be audited against them. Status values: **accepted**,
**deferred** (with a trigger), **rejected**.

---

## ADR-001 — Modular monolith in a Cargo workspace

**Status:** accepted
**Context:** The mission asks for ~20 crates but also warns against over-engineering before
measurement. The reference ecosystem contains both extremes: single-module clients (`innertube`,
`Unofficial-YoutubeApi`) and many-module services (Piped-Backend, MediaServiceCore v1/v2 split).
MediaServiceCore's v1/v2 split shows the cost of premature module boundaries: two parallel
generations with duplicated services and no deprecation path.
**Decision:** One binary-serving crate per concern is created **only** where a boundary is real
(domain types, provider engine, storage, edge). Modules that would only exist to justify the tree
(`vydra-video`, `vydra-channel`, `vydra-playlist`, `vydra-comments`, `vydra-live`,
`vydra-captions`, `vydra-discovery`) are implemented as **modules inside `vydra-engines`** for now,
with a documented extraction path. `vydra-search` is a module too, until a second search source
exists.
**Consequences:** 12 crates instead of 20; compile times and dependency graph stay manageable; the
public interface (`vydra-domain`) and the provider contract (`vydra-providers`) — the two boundaries
that actually matter — are enforced by the compiler. Moving a module to its own crate later is a
mechanical change (no cyclic dependency exists because `vydra-engines` may not be depended on by
`vydra-providers`).
**Alternatives rejected:** (a) 20 crates from day one — unjustified boundary churn, longer CI;
(b) single crate — loses the provider/domain separation that is the whole point.

---

## ADR-002 — Two model families: parse models and domain models

**Status:** accepted
**Context:** youtubei-rs (renderer-mirror models) decayed within months; YouTube.js carries 449
renderer classes; MediaServiceCore's `videoinfo/models` breaks on renames. Conversely,
NewPipeExtractor/rustypipe normalize early and stay stable. Requirement 6 also forbids a provider
from controlling the public response format.
**Decision:**

- *Parse models* (`crates/vydra-providers/src/innertube/dto/**`): `serde` structs mirroring only the
  renderer keys we consume, all fields `Option`/`#[serde(default)]`, `#[serde(deny_unknown_fields)]`
  never used. They are `pub(crate)` and never leak.
- *Domain models* (`crates/vydra-domain`): provider-neutral, fully typed, with explicit
  `Provenance` and availability metadata.

Mapping between them happens in a dedicated `map` module per resource and emits a `ParseReport`.
**Consequences:** a YouTube rename touches one DTO + one mapper, and unknown keys are counted instead
of crashing. Public types are stable independent of provider drift. Cost: two structs per resource.
**Alternatives rejected:** JSONPath-style reflection projection (MediaServiceCore) — no compile-time
safety in Rust and worse debuggability than explicit mapping.

---

## ADR-003 — `Transport` trait instead of a concrete HTTP client

**Status:** accepted
**Context:** `NewPipeExtractor/downloader/Downloader.java` proves the value: extraction code is
transport-agnostic, tests inject recorded responses. rustypipe, yt-dlp and YouTube.js do the same
with different mechanisms.
**Decision:** `vydra-providers::Transport` is an object-safe async trait:

```rust
#[async_trait] // or native async-trait-in-trait; see crate docs
pub trait Transport: Send + Sync {
    async fn execute(&self, req: TransportRequest) -> Result<TransportResponse, TransportError>;
}
```

`TransportRequest` is a *validated* value object: method, absolute `Url`, allowlisted headers,
bounded body, timeout hint. `TransportResponse` is `{ status, headers, body: Bytes, final_url,
elapsed }` — immutable, like NewPipe's `Response`. Production impl: `ReqwestTransport` with pooled
client, per-host limits, size caps, SSRF guard. Tests: `ReplayTransport`, `FailingTransport`,
`SlowTransport`.
**Consequences:** every parser test is deterministic and offline; provider code cannot bypass
timeouts/size limits; SSRF protection is centralized in one place.

---

## ADR-004 — Session bootstrap as a service with single-flight refresh

**Status:** accepted
**Context:** `Session.kt` (MediaServiceCore) caches one session per player id and invalidates on
`isUnplayable`; `Session.ts` caches per options with an `ICache`; yt-dlp caches client config.
Without caching, every request pays an extra bootstrap round-trip.
**Decision:** `SessionProvider` owns `Arc<SessionState>`:

- bootstrap order: cached file/Redis value → `sw.js_data` → HTML `ytcfg` → hardcoded fallback client;
- single-flight via `tokio::sync::Mutex<Option<Session>>` + generation counter so concurrent callers
  trigger one refresh;
- TTL (default 6 h) + explicit `invalidate()` on evidence of a stale session (`LOGIN_REQUIRED`
  repeated N times, `WrongResult`, 403 on player);
- session data is versioned by `client_profile_hash` so a config change invalidates it.
**Consequences:** bootstrap cost amortized; no thundering herd; stale sessions self-heal.

---

## ADR-005 — Tolerant parsing with an explicit `ParseReport`

**Status:** accepted
**Context:** rustypipe's `MapResult`/`DeserializationWarnings` and YouTube.js' parse warnings both
exist because YouTube silently changes/removes fields. Strict parsing turns a partial degradation
into an outage.
**Decision:** every parse returns `(T, ParseReport)` where `ParseReport` counts: unknown renderer
keys, skipped malformed items, missing recommended fields, and truncated content. A configurable
`strictness` (`lenient` default, `strict` for tests) turns any report into an error. Reports are
attached to tracing spans as fields and can be surfaced in `GET /status`.
**Consequences:** degradation is observable before it becomes an incident; fixtures can assert on
report contents (e.g. "this fixture must produce zero unknown keys").

---

## ADR-006 — No circumvention of anti-bot / integrity mechanisms

**Status:** accepted
**Context:** media URLs require `signatureCipher` deciphering, `n`-parameter transformations, and
PoToken/BotGuard (Section 6.4 of `EXTRACTION_METHODS.md`). Implementing them means executing Google's
obfuscated VM and defeating an integrity control.
**Decision:** Vydra does **not** implement signature deciphering, `n` transformation, BotGuard/PoToken
generation, captcha solving, or geo-restriction bypass. It:

1. exposes format *metadata* (`itag`, `mime`, `codecs`, bitrate, dimensions, size, DRM, audio track)
   and flags `playability.restricted = true` when URLs are cipher/`pot`-protected;
2. never returns a URL it has not verified to be directly playable — protected URLs are omitted with
   an explicit reason code;
3. documents the limitation in `docs/providers/MEDIA_LIMITATIONS.md` and in the API schema.

**Consequences:** Vydra is a *data* API, not a downloader/proxy. This is a deliberate scope boundary
that also keeps the project out of Piped/Invidious' most fragile subsystem. Any future, legally
reviewed integration must be an external, separately deployed component (as Invidious' companion),
behind a capability trait — never inline in the provider.

---

## ADR-007 — Provider engine: capabilities, health, bounded fallback

**Status:** accepted
**Context:** No reference project has a provider abstraction; fallback inside them is per-client and
ad hoc (`rustypipe` `WrongResult`, MediaServiceCore's v1/v2 dual paths). Requirement 6 demands
capability discovery, health, and *bounded* fallback.
**Decision:**

- `Provider` trait with typed request/response enums per resource group and a `capabilities()`
  method returning `CapabilitySet` (bitflags over `Resource × Feature`, e.g.
  `SEARCH_VIDEOS | SEARCH_FILTERS | VIDEO_CHAPTERS | COMMENTS_REPLIES`).
- `ProviderRegistry` performs selection: filter by capability, order by priority, skip unhealthy,
  then attempt at most `max_attempts` (default 2, excluding deterministic `Unavailable`).
- Health is a per-provider windowed error-rate + circuit breaker (closed → open → half-open) with
  jittered cooldown; state is in-process and (optionally) Redis-replicated.
- Fallback is *bounded and observable*: every attempt emits a span event; the response carries
  `sources: [{provider, latency_ms, outcome}]` in `meta`.
**Consequences:** adding Removed/HTML provider or a second vendor costs one impl + one registration.
A provider outage degrades latency, not correctness. Cost: two code paths to test (registry unit
tests + per-provider contract tests).

---

## ADR-008 — Opaque, signed cursors; never expose provider state

**Status:** accepted
**Context:** All references round-trip raw provider continuation tokens
(`SEARCH_CONTINUATION`, `{"continuation": …}`). Raw tokens leak provider internals, bind clients to
one provider, and let a caller replay arbitrary provider requests.
**Decision:** the public `cursor` is `base64url(json{provider, kind, token, iat}) + "." + hex(hmac)`.
Validation checks signature, kind, and age (default 24 h). Tampering yields `400 invalid_cursor`.
**Consequences:** provider changes are transparent to clients; cursors are not a free proxy; no
server-side storage needed.

---

## ADR-009 — One public error contract

**Status:** accepted
**Context:** References either swallow errors (Unofficial-YoutubeApi), return HTML (Invidious), or
expose internal exception names (Piped). Requirement 7 demands a standard error format.
**Decision:** every error response is:

```json
{
  "error": {
    "code": "provider_rate_limited",
    "message": "Upstream provider rate-limited the request.",
    "status": 429,
    "request_id": "01J…",
    "retryable": true,
    "retry_after_ms": 1500,
    "details": [{"field": "q", "issue": "too_long"}]
  }
}
```

`code` is a closed enum (documented in `docs/api/ERRORS.md`). Internal cause chains are logged, never
serialized. Retryable-ness is derived from the error class, not from the message.
**Consequences:** clients can branch on `code`; observability can count error classes; no leakage of
stack traces or upstream HTML.

---

## ADR-010 — Three cache classes, explicit TTLs, no caching of failures

**Status:** accepted
**Context:** See `ARCHITECTURE_COMPARISON.md` §4. Conflating session state, derived artefacts and
responses is the most common caching bug in this domain (Invidious' Postgres-as-cache, Piped's
absent metadata cache).
**Decision:** `vydra-cache::Cache` trait with `Class` namespace:

| Class | Default TTL | Contents | Notes |
|---|---|---|---|
| `session` | 6 h | api key, visitor data, client version | single-flight, versioned |
| `bootstrap` | 24 h | `ytcfg`, consent state | survives restart |
| `metadata` | 5 min | video/channel/playlist docs | stale-while-revalidate |
| `search` | 60 s | search pages | opt-in, off by default |
| `static` | ∞ | itag table, filter params | compiled-in, not Redis |

Never cached: `playability`, errors, 4xx/5xx, cursors. Cache values carry `provider_version` and are
invalidated when the provider's config hash changes.

---

## ADR-011 — Toolchain, MSRV, dependency policy

**Status:** accepted (with a caveat recorded in `docs/PROGRESS.md`)
**Context:** Mission requires Rust 2024 edition, Tokio, Axum, Tower, Reqwest, Serde, SQLx, Redis,
OpenAPI. Edition 2024 requires Rust ≥ 1.85.
**Decision:** `rust-toolchain.toml` pins `stable`. The MSRV is **1.88** as of Phase 2026-11-13: the
previous floor (1.85) forced `time` to resolve to 0.3.45, which carries RUSTSEC-2026-0009 (stack
exhaustion on parsing); every fixed `time` release (≥ 0.3.46) requires 1.88. MSRV enforced in CI with
`cargo msrv verify`-style job using `cargo-hack`). Dependencies are pinned by caret ranges; an
`xtask`-free `deny.toml` enforces license allowlist (MIT/Apache-2.0/BSD/ISC/Unicode/MPL-2.0) and
bans copyleft **runtime** dependencies, which also prevents accidental reuse of AGPL/GPL code.
**Caveat (updated 2026-11-13):** this sandbox still has no Rust toolchain and no `crates.io`
access, so nothing is compiled locally. The workspace *has* now been compiled: GitHub Actions run
43448221721 is green for `fmt`, `check`, `clippy -D warnings`, `test` (14 packages), `audit`,
`deny`, `secrets`, `openapi` and `docker`. See `docs/PROGRESS.md` §"Phase 4.1". The 1.88 floor is
therefore a verified constraint, not a guess: `time` 0.3.55 is the version that resolves under it.

---

## ADR-012 — API-key auth with hashed keys and Redis-backed quotas

**Status:** accepted (implementation: Phase 8)
**Context:** Requirement 5/9 (quotas, rate limiting, secret protection). None of the reference
projects implement API keys (they are either libraries or unauthenticated public instances).
**Decision:** keys are `vydra_<prefix>_<secret>`; only `sha256(secret)` + prefix are stored
(Postgres `api_keys`). Verification is constant-time. Quota counters live in Redis (`INCR` + `EXPIRE`
window) with an in-process fallback for single-node deployments. Per-key and per-IP limits are both
enforced; quota exhaustion returns `429 quota_exceeded`. Keys are never logged (redaction layer in
`vydra-observability`).
**Consequences:** no plaintext key material at rest; usable defaults in Docker Compose
(`VYDRA__AUTH__BOOTSTRAP_KEY` for local dev only, with a startup warning).

---

## ADR-013 — Testing strategy: fixtures are the contract

**Status:** accepted
**Context:** See `ARCHITECTURE_COMPARISON.md` §6. The mission forbids fabricated results and requires
deterministic offline tests.
**Decision:**

1. **Fixture tests** (`vydra-providers/tests/`): replay recorded/synthetic payloads through the real
   parser and assert on domain output *and* `ParseReport`. Fixture files declare provenance in a
   sidecar `.meta.json` (`{"source": "synthetic_schema_reference" | "captured_live", "captured_at": …,
   "client": "WEB", "endpoint": "search"}`). Synthetic fixtures are never described as captured.
2. **Contract tests**: one suite parameterized over every registered `Provider` so a new provider must
   satisfy the same invariants (never panic, always return `sources`, respect cursor rules).
3. **Property tests** (`proptest`) for `vydra-common` parsers (duration/count/ids) and cursor
   round-trip/HMAC tamper resistance.
4. **Fuzz-ish robustness tests**: truncated JSON, unknown renderer keys, HTML instead of JSON, huge
   integers, unicode titles, 2 MB bodies.
5. **Live tests** are `#[ignore]` and never reported as passing evidence.

---

## ADR-014 — Observability: spans, extraction events, redaction

**Status:** accepted
**Context:** rustypipe's `report.rs` and Piped's Sentry integration show diagnostics matter; none of
the references have structured extraction telemetry.
**Decision:** `tracing` spans per request (`http.request`) and per provider attempt
(`provider.call`), with fields: `provider`, `endpoint`, `client`, `status`, `latency_ms`,
`parse_warnings`, `fallback_index`, `cache` (`hit|miss|stale`). Metrics: `vydra_provider_requests_total`,
`vydra_provider_latency_seconds` (histogram), `vydra_provider_circuit_state`,
`vydra_parse_warnings_total`, `vydra_cache_events_total`, `vydra_quota_denied_total`. A redaction
layer strips `Cookie`, `Authorization`, `X-Goog-*` values and any `*.secret` config key from logs.

---

## ADR-015 — Deferring NATS JetStream and OpenSearch

**Status:** deferred
**Context:** Requirement 4 lists them "when justified by workload".
**Decision:** not in v1. Triggers to revisit:
- **NATS JetStream**: when (a) more than one worker replica must coordinate cache warming or
  invalidation, or (b) burst metadata refreshes exceed 1k/min measured.
- **OpenSearch**: when local search/history features require ranking over stored documents
  (>10⁶ documents or full-text over titles/descriptions).
Until then: Postgres for durable state, Redis for ephemeral state, in-process workers.
**Consequences:** fewer moving parts; the `worker` app is a plain periodic loop with a lease row in
Postgres (no broker needed).

---

## ADR-016 — Data honesty rules (never fabricate, always attribute)

**Status:** accepted
**Context:** Requirement 6/9 and execution rule 7. The HTML-scraping reference demonstrates the
failure mode: it renders localized strings into numbers with locale heuristics and no provenance.
**Decision:**

1. Every document carries `source: {provider, endpoint, client, fetched_at, cached}` and
   `parse_warnings: N`.
2. Optional fields are `Option<T>`; `null` means *unknown*, `""`/`0` are never used as substitutes.
3. Parsed-from-human-text values carry `precision: exact | approximate` and keep `raw`.
4. Timestamps are RFC 3339 UTC; durations are integer seconds; counts are `u64`; money/ratings are
   absent rather than guessed.
5. `unavailable` reasons are enumerated, never free-form, and never conflated with 404.

---

## ADR-017 — CI is the compiler of record, and it must be observable

**Status:** accepted (Phase 4.1)
**Context:** No Rust toolchain exists in the development sandbox and `crates.io` is unreachable, so
`cargo` only runs on GitHub Actions. Job logs cannot be downloaded from the sandbox either — the
only retrievable channel is the check-run API. That combination made three failures invisible for
several runs: diagnostics were grepped as `^error` although `cargo --message-format=short` prints
`path:line:col: error[...]`; annotations were emitted from a step the runner skips when the build
step is killed; and a workspace-wide `cargo test` was killed after ~3 minutes with nothing printed.
**Decision:**

1. Every CI step that can fail runs `cargo` through `tee` and calls `scripts/ci/report.sh` **in the
   same step**, so annotations survive a nonzero exit.
2. `report.sh` matches real short-format diagnostics, panics and `could not compile` context, and
   falls back to the log tail rather than publishing nothing.
3. Tests run as a `fail-fast: false` matrix of one job per package, each capped at 240 s, with
   `scripts/ci/test-package.sh` naming any test that exceeds the cap (opt-in `DIAGNOSE_HANGS=1`
   probes every test individually).
4. No result is reported as passing unless a run id is attached.
**Consequences:** an eleven-minute green run instead of a five-minute opaque one; failing tests are
now named by the CI system itself. The trade-off is more jobs (14) and a longer run.

---

## ADR-018 — No upstream credentials compiled into the binary

**Status:** accepted (Phase 4.1, closes finding A-01)
**Context:** The first implementation shipped a hardcoded web API key literal
(`FALLBACK_WEB_API_KEY`). A shared key baked into a public binary is a credential leak with an
attribution problem: every deployment shares one quota identity, and rotating the key requires a
release.
**Decision:** every `ClientProfile` carries `api_key: None`. A key may only arrive from
(a) the session bootstrap (`sw.js_data` or the watch page, at runtime, per deployment) or
(b) `VYDRA__PROVIDERS__INNERTUBE__API_KEY` as an operator override. The literal was purged from
the working tree *and* from history; `secrets` (gitleaks) fails the build if one returns.
**Consequences:** a cold start now performs one bootstrap request before the first content request;
if the bootstrap fails and no override is set, provider calls fail closed with
`provider_unauthorized` rather than silently using somebody else's quota.

---

## ADR-019 — `Cargo.lock` is not committed; CI and Docker resolve it

**Status:** accepted, with a recorded follow-up
**Context:** The repository intentionally does not commit a lock file (it cannot be generated in the
sandbox — `index.crates.io` is unreachable), but `cargo audit`, `cargo deny` and the Docker build
all require one; the original `Dockerfile` did `COPY Cargo.lock` and could not build at all
(finding A-18).
**Decision:** `audit`/`deny` jobs run `cargo generate-lockfile` first; the `Dockerfile` builds from
`rust:1-slim` and resolves dependencies during the build. Caret ranges in the workspace manifests,
`deny.toml` for licences/advisories, and `cargo audit` in CI are the compensating controls.
**Follow-up:** committing a generated `Cargo.lock` is the correct end state for a deployed binary —
reproducible builds and a lock that `cargo audit` can check without a network resolve. Doing it
requires one real `cargo generate-lockfile` run, which this sandbox cannot perform. Until then the
lock policy is a known, documented limitation rather than a silent one.
