# Progress and Verification Status

Last updated: 2026-11-13. This file is the authoritative answer to "does it work?".

## Blocked verification (read first)

The development sandbox for this branch provides:

- no Rust toolchain (`cargo`/`rustc` absent; `static.rust-lang.org` and `index.crates.io` are
  unreachable, and the two binaries that appeared in `/usr/local/bin` were dead macOS builds),
- no access to `www.youtube.com` or `googleapis.com`.

**Compilation and test authority is GitHub Actions**, which is available and is where every
`cargo` command actually runs. The workflow (`ci.yml`) was extended in Phase 4.1 so that rustc,
clippy and rustfmt diagnostics are converted into check-run annotations
(`scripts/ci/annotate.sh`), because job logs cannot be downloaded from the sandbox.

Consequences, stated plainly:

1. **Nothing in this repository has been compiled in the sandbox.** Local verification was limited to
   manifest consistency checks, a tree-sitter parse of every Rust file, `redocly lint` on the OpenAPI
   document, and `bash -n` on CI helper scripts. See
   `docs/security/PROVIDER_ENGINE_AUDIT.md` §0 for the exact method.
2. **No test result is reported as passing unless a CI run says so.** Every claim below names the run
   it came from; unexecuted tests stay labelled unexecuted.
3. **No upstream request has ever been made.** Anything about live YouTube behaviour is derived from
   reading the reference implementations (`docs/research/`), not from observation.
4. Because of (3), capabilities that depend on unverified encodings are **not declared** by default
   (see "Conservative capability policy").
5. The CI status of any commit is a moving target; the authoritative source is the Actions run for the
   current head of `arena/06c8b7fc-vydra`.

## Phase status

| Phase | State | Evidence |
|---|---|---|
| 1 — Multi-repository research | complete | 5 documents in `docs/research/`, 12 repositories cloned and read |
| 2 — Architecture design | complete | `ARCHITECTURE.md`, `PROVIDER_INTERFACE.md`, `API_CONTRACTS.md`, 16 ADRs |
| 3 — Rust foundation | written, uncompiled | 12 crates, workspace manifests, CI, Docker, migrations |
| 4 — Provider engine | **audited, remediated, verification in CI** | contract, transport, InnerTube search/player/captions, registry, fixtures; 20 findings closed (see `docs/security/PROVIDER_ENGINE_AUDIT.md`) |
| 4.1 — Provider engine audit | in progress | `PROVIDER_ENGINE_AUDIT.md` + `PROVIDER_ENGINE.md`; remaining blockers are CI-only |
| 5 — Search engine | written for mixed search only | type-scoped search gated behind an opt-in flag |
| 6 — Metadata engines | video + chapters + captions metadata | channels, playlists, comments **not implemented** |
| 7 — Advanced modules | not started | live, music, trending, related, discovery |
| 8 — Infrastructure | skeleton only | Postgres schema + repository stub; Redis/NATS/OpenSearch deferred (ADR-015) |
| 9 — Hardening | not started | security model documented, not tested |

## Implemented (code exists)

- `vydra-common`: validated ids, localized count/duration parsing, signed cursors, pagination.
- `vydra-errors`: provider error taxonomy with fallback eligibility, public error contract.
- `vydra-config`: env-layered config, secret redaction, production guardrails.
- `vydra-domain`: provider-neutral models with provenance/availability/precision.
- `vydra-providers`: `Transport` (reqwest + replay), InnerTube session bootstrap, request/context
  building, search + player parsing, mapping, capabilities, parse reports.
- `vydra-provider-registry`: capability selection, sliding-window health, circuit breaker,
  bounded fallback, attempt accounting.
- `vydra-engines`: search/video/chapters/captions orchestration with caching and cursor minting.
- `vydra-auth`, `vydra-cache`, `vydra-observability`, `vydra-database`: keys/limits, cache contract
  and in-memory implementation, tracing/metrics, Postgres pool + migrations.
- `vydra-api`: envelope, error mapping, request-id/auth/rate-limit middleware, search + video routes
  mounted by capability.
- `apps/api-server`, `apps/worker`.

## Not implemented (do not pretend otherwise)

- Channel metadata / videos / playlists / about.
- Playlist documents and items.
- Comments (thread + replies) — no provider, no endpoint.
- Related videos (`next` endpoint) — returns `capability_unavailable`.
- Live metadata and live chat.
- Trending, categories, recommendations, music search.
- Transcript fetching (`timedtext`) — metadata only.
- Redis cache implementation, quota counters in Redis, OpenTelemetry export, worker jobs.
- Benchmarks, load tests, Kubernetes manifests.

## Conservative capability policy

The InnerTube provider declares only what does **not** depend on unverified encodings:

| Capability | Declared | Why |
|---|---|---|
| `search_all` | yes | `{"query": …}` needs no params token |
| `video_metadata`, `video_chapters`, `video_formats_metadata`, `video_subtitles_metadata`, `captions_metadata` | yes | stable player response shape |
| `search_filters`, `search_videos`, `search_channels`, `search_playlists` | **no** (opt-in) | depend on the protobuf `params` builder in `innertube/params.rs`, which is not live-verified |
| everything else | no | not implemented |

Route mounting follows from this table, so `/api/v1/search/videos` is absent unless the operator
enables unverified params. This is the mechanism behind execution rule 7 ("only expose endpoints when
the underlying functionality is implemented and tested").

## Tests added in Phase 4.1 (run in CI)

- transport: userinfo/port/fragment/IP-literal/scheme rejection, header-count and body-size bounds.
- registry: half-open success closes, failed probe re-opens, saturation fails fast, token bucket,
  concurrent successes keep the breaker closed, duplicate ids rejected, observer receives attempts.
- engines: type-scoped search filters foreign item kinds.
- config: `VYDRA__PROVIDERS__ENABLE_UNVERIFIED_FILTERS` parsing and defaults.
- api: filter opt-in gate, malformed filter values, deduplicated feature lists, locale normalisation.
- observability: every metric vector is registered and renders; sensitive headers are redacted.

## Tests written but not executed

- `vydra-common`: id validation/injection rejection, view-count and duration parsing (including the
  documented false-positive cases), property tests (no panic, round-trip), cursor HMAC/expiry/tamper.
- `vydra-providers`: transport allowlist/SSRF/header-injection, replay transport, parse-report
  budgets, renderer discovery, HTML `ytInitialData` recovery, session caching and error
  classification, player mapping (protection flags, availability, chapters, captions), and a
  fixture-driven mixed-search test using a **synthetic** payload.
- `vydra-provider-registry`: fallback on transient errors, no fallback on content facts, attempt
  bounding, circuit opening, registry-enforced timeout.
- `vydra-engines`: signed cursor substitution and validation, metadata caching, search caching off by
  default.
- `vydra-auth`: key hashing/verification, rate limiting, constant-time comparison.
- `vydra-api`: request id shape, query validation, meta exposure.

## Next steps (in order)

1. Drive the CI pipeline to green (`fmt`, `check`, `clippy`, `test`, `audit`, `deny`, `openapi`,
   `docker`) and record the run id here once all jobs pass.
2. Live-verify InnerTube bootstrap (`sw.js_data` + `ytcfg`) and the `params` encoder; only then enable
   `search_filters`/type-scoped search by default and capture real fixtures.
3. Channels + playlists (browse endpoint), then comments (`next` + continuations).
4. Redis cache + quota; OpenTelemetry; worker jobs with a Postgres lease.
5. Benchmarks + load tests; publish measured numbers instead of claims.
