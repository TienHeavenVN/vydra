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

## ADR-019 — `Cargo.lock` is committed (superseded decision, Phase 4.2)

**Status:** superseded by the decision below (originally: "`Cargo.lock` is not committed; CI and Docker
resolve it")

### Final decision (Phase 4.2): the lockfile is committed

**Context.** ADR-019 originally declined to commit a lockfile, because the development sandbox cannot
reach `index.crates.io` and therefore cannot run `cargo generate-lockfile`. `cargo audit`, `cargo deny`
and the Docker build all need a lock, so CI generated a throwaway one per job and the image resolved
its graph at build time. That is a *documented* limitation, not a safe one: a deploy could compile a
dependency graph nobody tested, and an advisory check performed on a different resolution than the one
that ships is not an advisory check at all. Vydra ships deployable binaries (`apps/api-server`,
`apps/worker`), which is exactly the case where a lockfile belongs in the repository.

**Evidence.** A one-shot CI job (`lockfile`) did the work the sandbox cannot:

1. `cargo generate-lockfile` resolved the workspace (run
   [43456657431](https://github.com/TienHeavenVN/vydra/actions/runs/43456657431), job `lockfile`,
   step `resolve and validate` — **success**).
2. `cargo check --workspace --locked` then proved the resolved graph still compiles
   (same step, success). Resolution respected the workspace MSRV: the log records
   `Adding uuid v1.26.1 (available: v1.29.2, requires Rust 1.89.0)` — the newer version was not taken
   because `rust-version = "1.88"`.
3. Only then was the file committed (`Cargo.lock`, 3 172 lines, commit `5276f7c`, author
   `github-actions[bot]`). The job was removed in the same pull request.

**Decision.** `Cargo.lock` is tracked. Consequences applied here:

* `Dockerfile` copies `Cargo.toml` **and** `Cargo.lock` and builds with `--locked`, so the image builds
  the graph CI validated.
* `cargo audit` / `cargo deny` in CI now read the committed lock (the `cargo generate-lockfile` step is
  still harmless, but the committed file is the reference).
* Dependency updates are now explicit: bumping a version means changing `Cargo.lock` in a commit that
  CI validates with `--locked`.

**Rejected alternatives.** (a) Keep resolving at build time — irreproducible, and makes advisory
results non-transferable. (b) Commit a lockfile generated outside CI — no evidence it compiles, and the
sandbox cannot run cargo at all. (c) Leave the job in place permanently — a CI job that pushes to the
branch on every run races with every other push, and the file only needs generating when manifests
change.

**Residual.** The lockfile was produced by `stable` at the time of the run; a future MSRV bump may
change it. That is a normal, reviewable diff, not a hidden one.

---

## ADR-019 (historical) — `Cargo.lock` is not committed; CI and Docker resolve it

**Status:** superseded
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

---

## ADR-020 — A playability verdict is a value, not an HTTP status

**Status:** accepted (Phase 5A)
**Context.** `POST /youtubei/v1/player` answered **HTTP 200** on all eight Phase 5A requests, including
the three refusals: `UNPLAYABLE` ("The page needs to be reloaded.") with a full metadata document,
`LOGIN_REQUIRED` ("Sign in to confirm you're not a bot") and `ERROR` ("This video is unavailable") with
no document at all. The HTTP status is therefore uninformative, and the parser has to answer two
independent questions: *is there a document?* and *what does upstream say about playing it?*

Before this decision the code answered them with one branch and got three outcomes wrong: a substring
match on `"age"` inside `"page"` invented `age_restricted`; a refusal without `videoDetails` was reported
as `provider_parse` (blaming the parser for an upstream decision); and a response with no verdict at all
became `Available`, which would have been cached and served as playable.

**Decision.**

1. **The verdict is mapped first, and it never silently becomes success.** `OK` → `available`; a
   recognised restriction → `unavailable { reason }`; an unrecognised status → `unavailable { reason:
   other }` plus a `playability:<STATUS>` message and a `tracing` warning; a missing verdict →
   `Availability::Unknown` (new variant) plus a counted `missing_field`.
2. **The document is returned when it exists, regardless of the verdict.** A `UNPLAYABLE` answer that
   carries `videoDetails` and `microformat` is real metadata; discarding it would be as wrong as calling
   it playable. The API serves it with an explicit `availability`.
3. **No metadata means a typed error, not an empty document.** A refusal without `videoDetails` becomes
   `ProviderError::Unavailable { reason, message }` → `409 content_unavailable`, which is deterministic
   and therefore never fallback-eligible — another provider must not be asked for a different video.
4. **Reason selection is whole-word/phrase over `reason`, `messages[*].text` and `errorScreen`.** Text
   is used for classification only and is never copied into a document; the error message carries at
   most 200 printable characters of the `reason` phrase.
5. **`Availability::Unknown` is additive.** `{"state":"unknown"}` on the wire, `is_available() == false`,
   never cached. No existing variant changed its wire form.

**Rejected alternatives.** (a) Map every non-OK verdict to `409` and drop the metadata — loses verified
data, and makes `/videos/{id}` useless for the most common real case (content exists, playback refused).
(b) Keep the old "200 with `availability`, default `Available` when absent" — it manufactured a success
upstream never claimed. (c) Return `404` for `ERROR` — upstream does not distinguish deleted from
never-existed, so `other` is the honest reason and `409` keeps existence and availability separate
(ADR-016).

**Evidence.** `docs/research/data/player-probe-43549141233/` (where the `age_restricted`
misclassification is visible in the recorded provider projection) and
`docs/research/data/player-probe-43549736295/` (the fixture source);
`docs/research/PLAYER_RESPONSE_ANALYSIS.md` §4–5; `crates/vydra-providers/tests/player_parser.rs`.

**Residual.** Wording-based classification can lag a new upstream phrasing. That failure mode is
deliberate: an unrecognised phrase maps to `other` and surfaces the raw status rather than guessing a
restriction. A Rust client matching exhaustively on `Availability` must handle the new variant; there is
none in this workspace.

## ADR-021 — A video document states where each value came from

**Status:** accepted (Phase 5B)
**Context.** The player response duplicates facts. `videoDetails.lengthSeconds` and
`microformat.playerMicroformatRenderer.lengthSeconds` disagree in the very first live capture Vydra made
(213 vs 214 s), `viewCount` appears in both, and the channel name appears in both. Before this decision the
mapper read one section per field, silently: a reader had no way to know whether `duration_seconds: 213`
came from the playback authority or from a fallback, and no way to discover that upstream had stated two
answers at all. The same capture also carried `likeCount`, `uploadDate`, `isUnlisted` and `isFamilySafe`,
none of which was mapped — data present in the response and absent from the document, with no note saying
so.

**Decision.**

1. **A fixed precedence table**, documented in `docs/research/VIDEO_METADATA_CONTRACT.md` §3:
   `videoDetails` primary, `microformat` fallback, single-source fields (`likeCount`, `category`,
   `publishDate`) read where they exist.
2. **Every substitution and every disagreement is recorded** in `data.evidence`
   (`sections[]`, `fallbacks[]`, `conflicts[]` with both values and both sources). The accepted value is
   served; the rejected one is preserved as text so the disagreement stays auditable.
3. **Absent means "upstream did not state it"; present-but-unreadable is a counted loss.** The first is
   silent, the second produces `missing_field:<section>.<field>` and fails strict parsing.
4. **Normalisation is lossy, so the original is kept**: `Publication` holds the UTC instant *and* the
   upstream string plus its offset.
5. **A malformed member degrades one field, not the document**: every member is read through a helper
   instead of a concrete deserialization type.

**Rejected alternatives.** (a) Merge both sections into one "best value" per field — unfalsifiable, and it
hides the fact that upstream itself is inconsistent. (b) Fail the document on a disagreement — the response
is real; the disagreement is upstream's, not Vydra's. (c) Deserialize the microformat into a strict struct —
one wrong type would reject an otherwise complete document.

**Evidence.** Live capture `docs/research/data/player-probe-43549736295/` (the `213`/`214` disagreement,
`likeCount: 19503628`, `publishDate` with `-07:00`); tests in `innertube/map.rs` and
`tests/player_parser.rs`.

**Residual.** `evidence` grows with the response; it is omitted entirely when empty, so documents from a
minimal payload are unchanged on the wire.

## ADR-022 — Cache TTL follows the document's availability, not the endpoint

**Status:** accepted (Phase 5B)
**Context.** The metadata cache used one TTL (300 s) and cached only documents whose `availability` was
playable. Two facts were lost: content-fact unavailability (private, deleted, geo) was re-fetched on every
request even though it is stable for minutes, and a `live` document — viewer counts, end time, broadcast
state — was cached as if it were a published upload. Separately, cache entries were stamped with a
fingerprint of provider **ids** only, so bumping the InnerTube client version (which is part of the request
context and changes the body upstream returns) would have kept serving the previous client's documents.

**Decision.**

| Document | TTL | Reason |
| --- | --- | --- |
| `available` | `cache.metadata_ttl_secs` (300 s) | stable for minutes |
| `live` / `upcoming` | `cache.live_ttl_secs` (30 s) | changes while it airs |
| `unavailable` | `cache.unavailable_ttl_secs` (60 s) | reversible: private → public |
| `unknown` | never | the next request may get a verdict |
| any upstream error | never stored at all | an outage must never be remembered as unavailability |

The cache key is `video:{id}:{hl}:{gl}`; the client profile belongs to the entry fingerprint
(`ProviderRegistry::config_fingerprint`, id + client label) because it cannot change at runtime but does
change the response.

**Rejected alternatives.** (a) Keep one TTL — either live data goes stale for five minutes or published
metadata is re-fetched every 30 s. (b) Negative-cache errors — an outage would become a content fact.
(c) Put the client version in the cache key — it is constant per process; the fingerprint is the correct
scope. (d) Add Redis — measured requirement absent, ADR-015 stands.

**Evidence.** `vydra-engines` cache-policy tests, `vydra-provider-registry` fingerprint test,
`docs/research/VIDEO_METADATA_CONTRACT.md` §5.

**Residual.** Stale-while-revalidate is configured but not implemented (`MemoryCache` stale window is
zero); repopulating a hot key still costs one blocking upstream call.

## ADR-023 — A listing row is not a document: lockup rows keep `position: None` and `availability: Unknown`

**Status:** accepted (Phase 6)
**Context.** Current channel tabs and playlist pages render their rows as `richItemRenderer > content >
lockupViewModel` rather than `videoRenderer`/`playlistVideoRenderer`. The Phase 6 probe captured a live channel
Videos tab (run 43561555917) and its rows carry a validated `contentId`, a title, thumbnail sources, a duration
badge and a metadata row pair — and **no** `index` and **no** `isPlayable` field anywhere. Two tempting
shortcuts exist: compute the position from the row's offset in the page, and treat a missing `isPlayable` as
"available". Phase 5A already measured what the second shortcut costs (`Availability::Unknown` exists because
the mapper used to manufacture a success), and the first is worse: a continuation page starts at an arbitrary
offset, so a computed position would contradict the number the playlist owner sees.

**Decision.**

1. Lockup rows are mapped (id, title, thumbnails, duration badge, metadata row pair) instead of being counted as
   unmapped content, because a row shape this common is content, not noise.
2. `position` stays `None` when the row states none. It is never derived from the page offset, the request's
   limit, or the previous item.
3. `availability` stays `Unknown` when the row states no playability, and `PlaylistItem::is_playable()` returns
   false for it. `Unknown` is not cached as success and is not counted as a failure.
4. A duration comes only from a badge that parses as a clock value; a badge label ("Now playing", "LIVE") is
   never read as a length.
5. A row whose `contentId` fails `VideoId` validation is dropped with `skipped_item` and a reason — a lookalike
   string never becomes a video reference.

**Rejected alternatives.** (a) Count lockup rows as unmapped — that is what Phase 6 shipped first, and it made
both listing endpoints return zero items on a real page. (b) Compute positions from offsets — silently wrong on
every continuation page. (c) Emit `Available` for a row with no verdict — the exact defect Phase 5A exists to
prevent. (d) Drop rows without a title silently — they are reported as `missing_field` instead.

**Evidence.** `channel_parser::lockup_rows_map_into_a_channel_video_listing`,
`channel_parser::lockup_rows_survive_a_missing_metadata_block`,
`playlist_parser::lockup_entries_keep_an_unknown_position_and_an_unknown_availability`,
`playlist_parser::a_lockup_entry_without_a_title_is_reported_instead_of_fabricated`; fixtures
`browse_channel_videos_lockup.json`, `browse_playlist_entries_lockup.json` (synthetic, keyed like the live sample).

**Residual.** Only the channel Videos tab was sampled; if a playlist surface nests lockup rows differently the
by-key fallback may find nothing and the page reports `playlist_entries_missing` instead of fabricating entries.

## ADR-024 — A `@handle` is refused, not resolved

**Status:** accepted (Phase 6)
**Context.** `ChannelRef` models both a canonical `UC…` id and a `@handle`, because channel pages render vanity URLs
and search results carry handles. The first Phase 6 implementation sent `{browseId: "@veritasium"}` straight to
`browse`; the probe (run 43561555917) received `HTTP 400`. A handle is therefore not a browse id. Resolving one
would require a second request — a search, or the `about` tab — whose result is Vydra's guess about which channel
the caller meant, and whose id would differ from the id in the request path.

**Decision.**

1. `/api/v1/channels/{id}` accepts a canonical `UC…` id only; the OpenAPI pattern says so.
2. A handle is rejected with `400 validation_failed{field: "id", issue: "handle_not_supported"}` before any
   provider call, and the provider refuses it with a typed `Unsupported` error as defence in depth.
3. `ChannelSummary::handle` remains: a *document* may state its own vanity URL even though a *request* may not use
   one.

**Rejected alternatives.** (a) Search for the handle and take the first channel — a guess presented as a lookup.
(b) Fetch `youtube.com/@handle` and scrape the redirect — a second undocumented surface, and the redirect is exactly
what a bot check gates. (c) Silently rewrite the response id — a client that asked for `@x` and got `UCy` cannot
tell a redirect from a wrong answer.

**Evidence.** probe run 43561555917 (`browseId "@veritasium"` → `400`); `docs/research/CHANNEL_CONTRACT.md` §7.

**Residual.** Handle support returns when a handle→id resolution can be verified as an exact match (e.g. a page
whose stated vanity URL equals the requested handle), not before.

## ADR-025 — A comment page is addressed by a relayed token, and replies are a separate operation

**Status:** accepted (Phase 7)
**Context.** The comment section has no addressable endpoint: it is reached through a continuation token that only
a page which already rendered the section carries. Two orders ("Top", "Newest") are not request parameters either —
each is a menu entry carrying its own token. Replies are addressed by a third token that exists only inside the
comment page that rendered the thread. Every reference implementation therefore relays tokens; none constructs one.
The obvious shortcuts are all attractive and all dishonest: hard-code a `params` blob for an order (no reference
implementation does), derive a reply token from a comment id (undocumented encoding; a wrong token pages a
different thread and the client cannot tell), or treat "no comment section" as "no comments".

**Decision.**

1. `NextRequest` can express exactly two bodies — `{context, videoId}` and `{context, continuation}` — and no third.
   There is no `params` field. A token is validated (non-empty, ≤ 8 KiB, no whitespace/control characters) before
   the request is made.
2. The section token comes from the watch page's `itemSectionRenderer.targetId == "comments-section"`. Its absence
   is `UnavailabilityReason::CommentsDisabled`, served as `409 content_unavailable` with
   `details[0].issue: "comments_disabled"` — never an empty list, never a 404.
3. Sort order is positional in the page's own menu, and the token is relayed. `top` and `newest` are the only
   orders offered; an order the menu does not render is `Unsupported`, never approximated with the default.
4. Replies are a separate operation with their own cursor namespace, and the only way to reach one is a token a
   comment page rendered for that parent. When the thread is not among the first `REPLY_SCAN_PAGES` (3) default
   comment pages, the answer is `404 not_found` — a bounded cost stated as a refusal, not an unbounded scan.
5. A reply page never restates the thread's total. `total_reply_count` is the parent row's claim; `loaded_replies`
   is what a page carried; `has_more_replies` is whether upstream offered a continuation. Three facts, never merged.
6. Comment pages and reply pages share one cache class (`CacheClass::Comments`) with `cache.comments_ttl_secs`, and
   the cache key carries the video, the order, the token, the limit and the locale. A cursor page is never reused
   across orders, and a disabled verdict or a transient failure is never cached as an empty page.

**Rejected alternatives.** (a) Send a hard-coded `params`/token per order and claim sorting works because the
request returned HTTP 200 — the failure mode this project keeps refusing. (b) Build a reply token from a comment id
— silently wrong pages. (c) Flatten replies into the comment document and let the client page "the thread" — two
different cursors in one namespace, which is how a reply cursor eventually pages comments. (d) Cache a
comments-disabled verdict as an empty `CommentList` — makes a temporary state permanent and unrecoverable by the
client. (e) Fetch all replies eagerly to avoid a separate endpoint — unbounded requests behind one API call.

**Evidence.** `docs/research/COMMENTS_REQUEST_CONTRACT.md` (request shapes and the refusal table),
`docs/research/COMMENTS_RESPONSE_ANALYSIS.md` (both renderer shapes and the field table),
`tests/{comments_parser,request_contract}.rs`, `vydra-engines` comment tests.

**Residual.** A live probe has not yet confirmed the two-order menu or the reply-token path on today's upstream
(see `docs/reports/PHASE_7_REPORT.md` §7). A thread that appears only under `newest` is reported `not_found` by the
bounded scan; fixing that needs either a page-count budget the operator can raise or an order-scoped reply cursor,
and both are deferred until a live capture shows the scan failing.
