# Provider Engine Audit — Phase 4.1

**Scope**: the Provider Engine only — `vydra-providers` (transport, InnerTube, parser toolkit),
`vydra-provider-registry`, `vydra-engines`, the HTTP edge in `vydra-api`, `vydra-observability`, and
the configuration that drives them. No new features, no redesign: every change below either fixes a
confirmed defect or closes a gap that the Phase 4.1 acceptance criteria name explicitly.

**Date**: 2026-11-13 · **Branch**: `arena/06c8b7fc-vydra` · **Base**: `630437c`

---

## 0. How this audit was verified (read this before trusting anything below)

| Question | Answer |
| --- | --- |
| Is there a Rust toolchain in the build sandbox? | **No.** `rustc`/`cargo` are absent; the two binaries that appeared in `/usr/local/bin` were dead macOS builds copied into a Linux image and were removed. |
| Can the sandbox install one? | **No.** `static.rust-lang.org`, `index.crates.io` and `sh.rustup.rs` all fail at TLS from the sandbox (`SSL_ERROR_SYSCALL`). Only `github.com`, `api.github.com`, `codeload.github.com`, `registry.npmjs.org`, `pypi.org` are reachable. |
| Can the sandbox reach YouTube to verify provider behaviour? | **No.** Live upstream verification is impossible from here. Nothing in this report claims live provider behaviour. |
| So how is "does it compile / do tests pass" answered? | **GitHub Actions is the authoritative compile+test environment.** Every push runs `fmt`, `check`, `clippy`, `test`, `audit`, `deny`, `secrets`, `openapi`, `docker`. Results are quoted with the run id and the exact command. |
| How were compile errors retrieved, given job logs are unreachable from the sandbox? | CI was changed to run every cargo command through `tee` and to convert rustc/clippy/rustfmt diagnostics into GitHub **check-run annotations** (`scripts/ci/annotate.sh`), which *are* retrievable through the REST API. This is why the workflow grew an `annotate` step per cargo job. |
| What local verification *was* possible? | (a) `tomllib` validation of every manifest and of every `workspace.dependencies`/`workspace.package` reference; an automated cross-check of `crate::`-usage vs declared dependencies (this is how A-16 was found); (b) a tree-sitter parse of all 32 Rust files to catch syntax errors without a compiler; (c) `@redocly/cli lint` on the OpenAPI document; (d) `bash -n` on the CI helper script. |

> Consequence: findings marked **"confirmed"** were confirmed by reading the code and, where
> possible, by a failing CI job. Findings marked **"not executable here"** are code-level defects
> whose failing test exists but whose green state can only be observed in CI.

---

## 1. Findings

Severity: **S1** = correctness/security defect reachable in production, **S2** = contract or
robustness defect, **S3** = hygiene/maintainability.

| ID | Sev | Finding | Evidence | Status |
| --- | --- | --- | --- | --- |
| A-01 | S1 | A **hardcoded upstream web API key literal** shipped in `FALLBACK_WEB_API_KEY` and was used by all three client profiles. It is a shared credential, it trips secret scanning on every clone, and it silently binds every deployment to one quota identity. | `crates/vydra-providers/src/innertube/mod.rs` (pre-fix), CI job `secrets` failed with `Leaks detected` on run 43419351202 | **Fixed**: constant removed; `api_key: None` in every profile; key comes from session bootstrap or `VYDRA__PROVIDERS__INNERTUBE__API_KEY`. Literal also purged from git history and from the research notes. |
| A-02 | S1 | Outbound URL validation did not reject **userinfo**, **non-default ports** or **fragments**, and did not bound the header count or the request body size. `https://www.youtube.com@evil.example/` is the classic parser-confusion shape; a non-443 port is the classic "reach an internal service via an allowlisted name" shape. | `crates/vydra-providers/src/transport.rs::validate` | **Fixed** + 4 regression tests. |
| A-03 | S2 | `TransportRequest::validate` accepted a zero-byte response cap instead of rejecting it (a zero cap silently makes every response "too large"). | same | **Fixed** (rejects `max_response_bytes == 0`). |
| A-04 | S1 | **Every** route, including `/health` and `/ready`, sat behind the API-key middleware: orchestrators could not probe the service and Prometheus could not scrape it without a key. | `crates/vydra-api/src/lib.rs::router` (pre-fix) | **Fixed**: ops router (health/ready/status/metrics) is mounted unauthenticated, content router keeps auth + per-key/per-IP quota. |
| A-05 | S2 | `meta.sources` was **permanently empty**: the registry recorded every attempt in `FetchOutcome`, but engines discarded it and the edge built `Meta` with `sources: Vec::new()`. The public API therefore could not show fallback behaviour, which is the whole point of the field. | `crates/vydra-domain/src/common.rs` (`Provenance`), `vydra-engines`, `vydra-api::Meta` | **Fixed**: `Provenance.sources` added, populated from `FetchOutcome.attempts`, serialized through `Meta`, cleared on cache hits. |
| A-06 | S2 | Search/metadata **cache keys omitted `hl`/`gl`**, so a `hl=de` response could be served to a `hl=en` caller — localized titles *and* localized number formatting would silently cross over. | `vydra-engines::search/video` (pre-fix) | **Fixed**: locale participates in the key; cache hits clear `sources` and set `cached`. |
| A-07 | S2 | A **type-scoped search could return foreign item kinds**: the upstream mixed-search payload was passed through unfiltered, so `/search/videos` could answer with a channel. | `vydra-engines::search` | **Fixed**: items are filtered by `SearchKind`; regression test `type_scoped_search_filters_foreign_item_kinds`. |
| A-08 | S2 | Metrics vectors were created and **dropped immediately** (`let _ = …`), so `/metrics` exposed only a build counter — no provider errors, no latency, no circuit state. | `vydra-observability` (pre-fix) | **Fixed**: `Metrics` owns and registers six vectors; `vydra-api` bridges `ProviderObserver` into them. |
| A-09 | S2 | Anonymous callers shared **one global rate-limit bucket** (`"anonymous"`), so a single client could exhaust the budget for everyone. | `vydra-api::auth_middleware` (pre-fix) | **Fixed**: anonymous traffic is bucketed per peer IP via `ConnectInfo<SocketAddr>` (with the reverse-proxy caveat documented in §4). |
| A-10 | S2 | Search **filters were silently ignored**: `sort`/`since`/`duration`/`filter` were neither implemented nor rejected, so a caller asking for "view count, last week" got relevance-ordered results with no warning. Silently ignoring a filter is worse than refusing it. | `vydra-api::SearchQuery` (pre-fix) | **Fixed**: strict parsing (`400 invalid_request` on unknown values), `capability_unavailable` unless `VYDRA__PROVIDERS__ENABLE_UNVERIFIED_FILTERS=true`, and the same gate re-checked inside `vydra-engines` so non-HTTP callers cannot bypass it. |
| A-11 | S3 | `hl`/`gl` were passed through unvalidated into provider query strings and cache keys. | `vydra-api::SearchQuery` | **Fixed**: ASCII alnum + `-`, 2–10 chars, lowercased; `400` otherwise. |
| A-12 | S1 | **A failed half-open probe left the breaker stuck in `HalfOpen` with the probe slot consumed**: the old recorder only evaluated the "open" transition while `Closed`, so the circuit never re-opened and never admitted another probe — the provider was blackholed until restart. | `vydra-provider-registry` (pre-fix `ProviderState::record`) | **Fixed**: `HalfOpen` + failure ⇒ `open()` with doubled cooldown; covered by `circuit_transitions_closed_open_half_open_closed` and `half_open_probe_success_closes_the_circuit`. |
| A-13 | S2 | **Deterministic content facts counted as provider failures.** A private or deleted video incremented the failure window, so legitimate `Unavailable`/`NotFound` answers could open the breaker and take a healthy provider out of rotation. | same | **Fixed**: `record()` only applies failures that `affects_health()`; covered by a dedicated test. |
| A-14 | S2 | No **per-provider concurrency or rate gating**, and saturation was reported as `provider_circuit_open` — a misleading code that tells the caller to back off for the wrong reason and hides real quota pressure. | same | **Fixed**: `Semaphore` + token bucket per provider, `Error::RateLimited` for saturation/throttle, `ProviderCircuitOpen` reserved for actual breaker state. |
| A-15 | S3 | **Duplicate provider ids silently shadowed each other** (they share health state and gates, so the second provider is unreachable while appearing registered). | same | **Fixed**: duplicates are rejected at construction with `tracing::error!`. |
| A-16 | S1 | **The workspace did not compile**: `vydra-config` used `thiserror` and `vydra-engines` used `serde_json` without declaring them. | CI run 43420474500: `error[E0433]: cannot find module or crate 'thiserror' in this scope` at `crates/vydra-config/src/lib.rs:335` | **Fixed** (both dependencies declared). |
| A-17 | S3 | `clippy -D warnings` failure: redundant closure in `vydra-common/src/ids.rs`. | CI run 43420474500, job `clippy` | **Fixed**. |
| A-18 | S1 | **The Docker image could not build**: `COPY Cargo.lock` referenced a file that is intentionally not committed. | CI run 43419351202, job `docker`: `"/Cargo.lock": not found` | **Fixed**: `Dockerfile` builds from `rust:1-slim`, resolves dependencies at build time, and no longer copies a lock file or `rust-toolchain.toml`. |
| A-19 | S2 | The **audit job could not run** for the same reason (`cargo audit` requires a lock file), and `cargo-deny`'s failure was undiagnosable because the action swallows output. | CI run 43419351202, jobs `audit`/`deny` | **Fixed**: both jobs generate a lock file first and run the tools directly with `tee` + annotation. |
| A-20 | S3 | The OpenAPI document had no `security` scheme, so every operation failed `security-defined`; `/search/videos|channels|playlists` and `/videos/{id}/metadata` were served but undocumented; filters were undocumented. | `docs/api/openapi.yaml`; CI job `openapi` failed on run 43420474500 | **Fixed**: schemes + per-operation security, all mounted paths documented, filters marked opt-in; `npx @redocly/cli lint` is clean locally. |
| A-21 | S1 | **The cache could never hit.** Domain models omit unset optional fields (`skip_serializing_if`) but their `Deserialize` demanded them, so re-reading a cached document failed with `missing field published_at`. Every "cache hit" silently fell through to a fresh provider call — the cache was pure overhead, and the stored entry was re-serialized on every request. | CI run 43432463005, job `test`: `deserialize cached document: Error("missing field published_at", line: 1, column: 43)` at `vydra-engines` `video_metadata_is_cached_and_flagged` | **Fixed**: `#[serde(default)]` on all 56 `Option`/`Vec` fields that skip serialization, including the two multi-line `time::serde::rfc3339::option` attributes, plus a round-trip assertion in the caching test. |
| A-22 | S1 | **No channel handle could ever parse.** `ChannelHandle` applied the bare-id charset to the whole string, and `@` is not a bare-id character, so `ChannelRef::parse("@rustlang")` returned `Validation([handle: invalid_format])` for every input. Channel routing by handle was dead code that looked implemented. | CI run 43430663406, job `test`: `ids::tests::channel_ref_dispatches_on_prefix` panicked at `ids.rs:223` | **Fixed**: the sigil is stripped before charset validation (`@` + 1–39 URL-safe characters plus `.`, which YouTube permits). |
| A-23 | S2 | **The scraped API key was always discarded.** `find_api_key` required `len() > 20`, so the key it had just downloaded from `sw.js_data` failed the check, `or(session.api_key)` kept the configured override, and the "session bootstrap" step was decorative. A 20-character key (the documented shape `AIza` + 16) was rejected; real keys are 39. | CI runs 43444806970 / 43445452262, job `test`: `left: Some("AIzaTESTKEY")`, `right: Some("AIzaSWKEY0123456789")` | **Fixed**: `looks_like_api_key` = `AIza` + 12..=60 URL-safe characters, shared by both bootstrap paths. |
| A-24 | S2 | **Search results were returned in reverse document order.** `collect_renderers` pushed children onto a LIFO stack in source order, so the last sibling was visited first: a response whose first result was a video produced a playlist first. Ordering is user-visible (and the pagination cursor assumption of "append at the end" breaks). | CI run 43446155668, job `test`: `unexpected first item: Playlist(PlaylistSummary { id: PlaylistId("PLabcdefghijkl") … })` | **Fixed**: children are pushed in reverse, giving pre-order traversal in document order; asserted by `search_maps_mixed_items_and_skips_broken_ones`. |
| A-25 | S2 | **Layout-only renderers were counted as unknown content.** `twoColumnSearchResultsRenderer`, `sectionListRenderer` and `itemSectionRenderer` wrap every InnerTube response, so `ParseReport::unknown_renderers` — the signal meant to identify content Vydra cannot map yet — reported structure. One small fixture reported four "unknown" renderers, of which one was real, and the noise would swamp the metric in production. | CI run 43447524439, job `test`: `assert_eq!(report.unknown_renderers(), 1)` failed with `left: 4` | **Fixed**: `STRUCTURAL_RENDERERS` documents the containers that are traversed but not counted. |
| A-26 | S2 | **A test fixture hung the whole test job for 20 minutes.** The depth-budget test fed the previous value into *two* siblings per iteration, so building the fixture was exponential (2^200 nodes) and the test binary never returned. Because a hang publishes nothing, the CI job reported an opaque cancellation and hid every other result. | CI run 43439144664 (`test (vydra-providers)` in progress for 20 min, no output); located by the per-test probe in run 43444806970 (`exit 137, cap 20s`) | **Fixed**: a linear 200-deep chain (still 4× `MAX_DEPTH`) replaces the exponential one. CI now runs one job per package, caps each package at 240 s and names any test that exceeds the cap. |
| A-27 | S3 | **Dependencies that no code referenced were build failures.** `cargo` reports unused crate dependencies (workspace and crate level), and `clippy -D warnings` turns that into an error: `tokio`/`tower` in `vydra-api`, `serde`/`tokio`/`tracing` in `vydra-auth` and `vydra-cache`, `thiserror`/`tokio` in `vydra-database`, `vydra-config`/`serde`/`tracing` in `vydra-engines`, `serde_json` in `vydra-errors`, plus the `criterion`, `http`, `http-body-util`, `tower` and `vydra-database` workspace pins, `vydra-config`'s `tracing`, and `workspace.package` fields (`authors`, `keywords`, `categories`) that no member inherits. Both apps also lacked `[lints] workspace = true`. | CI runs 027d39b…8296c26, job `clippy` (`warning: unused dependency`, then `could not compile`) | **Fixed**: removed; every crate now inherits the workspace lint set. |

| A-28 | S1 | **DNS rebinding was open.** The allowlist was checked on the hostname and `reqwest` resolved afterwards, so a hostile or poisoned answer for an allowlisted name (the list contains wildcard suffixes such as `.youtube.com`) could return `127.0.0.1` or `169.254.169.254` and Vydra would connect to it, sending a JSON body and returning up to 4 MiB of the response. | `crates/vydra-providers/src/transport.rs` (pre-fix: no resolver hook) | **Fixed**: `GuardedResolver` implements `reqwest::dns::Resolve`; resolution happens before the socket exists, every prohibited address is dropped (IPv4, IPv6, and IPv4 wrapped in IPv6), an empty survivor set is `BlockedUrl`, and exactly the surviving `SocketAddr`s are handed to the connector, so the validated resolution *is* the connection. TLS hostname verification is untouched. Analysis + test table: `docs/security/DNS_REBINDING_ANALYSIS.md`. |
| A-29 | S1 | **The api key was never sent.** Session bootstrap downloaded the key into `SessionData.api_key` and every endpoint call then went out keyless (`…?prettyPrint=false`). Both reference clients transmit it (yt-dlp `youtube/_base.py:858-866` as the `key` query parameter; MediaServiceCore stores it in `Session.apiKey`). | `crates/vydra-providers/src/innertube/client.rs::post_json` (pre-fix) | **Fixed**: `endpoint_url` appends `&key=…` after `looks_like_api_key` validation (no interpolation of a hostile value), never as a header and never in a log line; asserted by three unit tests and by `tests/request_contract.rs`. |
| A-30 | S2 | **Ambient proxy support defeated the DNS guard.** The builder never called `.no_proxy()`, so `HTTPS_PROXY` would have made the proxy resolve the target itself, bypassing `GuardedResolver` — while the transport's own doc comment claimed "no ambient proxy". | `transport.rs::ReqwestTransport::new` | **Fixed**: `.no_proxy()`, with the trade-off documented in `DNS_REBINDING_ANALYSIS.md` §6. |
| A-31 | S2 | **Two robustness signals did not count.** A renderer-less search document and a player response without `playabilityStatus` were recorded with `ParseReport::warn`, which does not increment `ParseReport::count()`; `provenance.parse_warnings` therefore stayed `0` and strict mode accepted both documents. | `provider.rs::search`, `map.rs::map_player` | **Fixed**: `structural_problem("no_renderers")` and `missing_field("playabilityStatus", "status")`; covered by `tests/recorded_fixture.rs` and a `map.rs` unit test. |
| A-32 | S3 | **A fixture was named after an endpoint it never saw**: `recorded/search_web_2023-12-06.json` is a channel *browse* capture (`twoColumnBrowseResultsRenderer`), so the name implied a `/youtubei/v1/search` response that no source had. | `crates/vydra-providers/tests/fixtures/recorded/` | **Fixed**: renamed to `browse_channel_search_2023-12-06.json`, test renamed, `not_verified` list states the search container path stays inferred. |

### What was *not* found (positive results worth recording)

* No path where a **content fact triggers fallback**: `is_fallback_eligible()` is the single gate and
  `Unavailable`/`NotFound`/`Unsupported`/`InvalidRequest`/`CircuitOpen` are excluded.
* No place where a caller-supplied string reaches a provider URL without a typed newtype
  (`VideoId`, `ProviderId`) or an allowlisted, validated template.
* No redirect following at all (`reqwest` client is built without redirects, `https_only(true)`), so
  the "allowlisted host 302s to metadata service" class is structurally impossible.
* No sig/n-sig/PoToken/player-JS circumvention anywhere in the tree, and the experimental
  capabilities remain disabled by default.

---

## 2. Transport security model (and what an allowlist alone does not buy)

Controls, in the order they run (all **before** DNS resolution):

1. Scheme must be `https`.
2. Userinfo forbidden (`username` empty and no `password`).
3. Port must be absent or 443.
4. Fragment forbidden.
5. Host must match the allowlist (`youtube.com`, `www./m./music.youtube.com`,
   `youtubei.googleapis.com`, `www.googleapis.com`, `i.ytimg.com`, `yt3.ggpht.com`, …).
   IP literals can never match, which is what makes `169.254.169.254` and `[::1]` unreachable.
6. ≤ 32 headers; header names restricted to ASCII alphanumeric/`-`/`_`; values rejected on CR/LF.
7. Request body ≤ 256 KiB; response cap must be non-zero and is enforced while streaming (≤ 4 MiB
   by default), so a decompression bomb is bounded by the byte counter, not by the compressed size.
8. No redirects are followed; a redirect is a failed request, not a hop.

Residual, **unsolvable by an allowlist** (documented rather than hidden):

* ~~**DNS rebinding**~~ — **closed (A-28)**: `GuardedResolver` is installed via
  `ClientBuilder::dns_resolver`, validates every resolved address against a prohibited-range policy
  (IPv4, IPv6, and IPv4 wrapped in IPv6), fails closed when nothing survives, and returns exactly the
  addresses the connector dials. Ambient proxies, the other way to move the connection, are refused
  (`no_proxy()`, A-30). Redirects were already refused. Residual, unavoidable points (pooled
  connections, a mutually-trusted host answering hostilely, an explicitly configured proxy) are listed
  in `docs/security/DNS_REBINDING_ANALYSIS.md` §6.
* **TLS trust** relies on the platform trust store (`rustls-tls`); there is no certificate pinning.
* **Upstream-side quota** is invisible to us: a 429 is observed, a shadow-ban is not.
* **Fixture authenticity**: the recorded fixtures are *synthetic schema references*, not captured
  production payloads (`docs/providers/MEDIA_LIMITATIONS.md`). They prove parser behaviour, not
  upstream shape.
* **Sandbox limits**: no local compiler and no egress to YouTube, so "it compiles" and "upstream
  accepts our requests" are answered only by CI and by a deployment, respectively.

---

## 3. Registry invariants now enforced in code

| Invariant | Enforced by | Test |
| --- | --- | --- |
| Content facts never fall back | `is_fallback_eligible()` short-circuit in `ProviderRegistry::fetch` | `does_not_fall_back_on_content_facts` |
| At most `max_attempts` provider calls (clamped 1..=3) | `attempts_limit` | `attempts_are_bounded` |
| Breaker: `Closed → Open{until} → HalfOpen → Closed` | `ProviderState::{record, refresh, allow}` | `circuit_transitions_closed_open_half_open_closed`, `half_open_probe_success_closes_the_circuit` |
| Failed probe re-opens with doubled cooldown (capped) | `open()` + `cooldown().min(max_cooldown)` | same |
| Exactly one half-open probe in flight | `probe_in_flight` | `circuit_transitions_*` |
| Saturation fails fast (`RateLimited`), never queues | `try_acquire_owned` | `concurrency_limit_rejects_excess_instead_of_queueing` |
| Token bucket per provider (burst = rpm) | `TokenBucket` | `per_provider_rate_limit_is_enforced` |
| Concurrent successes never open the breaker | sliding window + `success_rate` | `concurrent_successes_do_not_open_the_breaker` |
| Duplicate provider ids rejected | `with_config` | `duplicate_provider_ids_are_rejected` |
| Observer sees every attempt and every transition | `ProviderObserver` | `observer_receives_attempts_and_circuit_changes` |

---

## 4. Unsolvable limitations / things deliberately not done

1. **Anonymous per-IP buckets** rely on the socket peer address. Behind a reverse proxy that is the
   proxy's address unless the proxy forwards a trusted client identity; Vydra does not parse
   `X-Forwarded-For` on purpose (spoofable without a trusted-proxy allowlist). Deployments that sit
   behind a proxy should rely on per-key quotas.
2. **No live upstream verification** of InnerTube behaviour, key discovery, continuations or
   localization. Everything InnerTube-related is marked experimental in `docs/PROGRESS.md`.
3. **No player-JS execution**; no sig/n-sig/PoToken. Consequently no stream URLs, no transcripts, no
   downloads.
4. **DNS rebinding** (see §2).
5. **No `Cargo.lock` in git.** For a binary workspace this is debatable; the trade-off is recorded in
   ADR-019. Audit/deny CI jobs generate the lock, and the Docker build resolves it. Committing a
   lock generated by a real `cargo` is a one-line follow-up once a toolchain is available.
6. **Formatting is not hand-verified.** Without rustfmt locally, formatting is produced by CI
   (`fmt-apply` job) and checked by the `fmt` job; the job is temporary and removed once the tree is
   formatted.

---

## 5. Verification status

| Command | Where | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | CI job `fmt` | **passing** (run 43448221721) |
| `cargo check --workspace --all-targets` | CI job `check` | **passing** (run 43448221721) |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | CI job `clippy` | **passing** (run 43448221721) |
| `cargo test --package <each>` (14 packages, one CI job each) | CI jobs `test (…)` | **passing**, 14/14 (run 43448221721) |
| `cargo audit`, `cargo deny check` | CI jobs `audit`, `deny` | **passing** (run 43448221721) |
| `gitleaks` | CI job `secrets` | **passing** after A-01 (run 43420663966) |
| `npx @redocly/cli lint docs/api/openapi.yaml` | local + CI job `openapi` | **passing** (local: 0 errors; CI green on run 43420663966) |
| `docker build` | CI job `docker` | **passing** (run 43448221721) |

## 6. Remaining blockers

Tracked honestly, with the reason each one cannot be closed from this sandbox:

1. ~~Full green CI run~~ — **closed**: run 43448221721 is green for all ten workflows jobs
   (22 after the temporary `fmt-apply` job was retired). See §5.
2. ~~**`Cargo.lock` policy**~~ — **decided and executed (Phase 4.2)**: the lockfile is generated by a
   one-shot CI job, validated with `cargo check --locked`, and committed. ADR-019 records the decision
   and the evidence.
3. **Live InnerTube verification** — needs egress to YouTube; blocked by the sandbox and by the
   deliberate refusal to bypass anti-bot measures.
4. ~~**DNS-rebinding hardening**~~ — **closed (A-28)**, see `docs/security/DNS_REBINDING_ANALYSIS.md`.
5. ~~**Fixture capture**~~ — **closed (Phase 4.2)** for the player and browse surfaces: three reduced
   subsets of MIT-licensed third-party captures, with provenance sidecars, plus three derived malformed
   documents. The global `/youtubei/v1/search` response and a search continuation remain unrecorded and
   are marked *inferred* everywhere they are used.

The authoritative, always-current statement of CI state is the Actions run for the head commit of
`arena/06c8b7fc-vydra`:
`https://github.com/TienHeavenVN/vydra/actions?query=branch%3Aarena%2F06c8b7fc-vydra`
