# Progress and Verification Status

Last updated: 2026-11-13. This file is the authoritative answer to "does it work?".

## Blocked verification (read first)

This working environment provides:

- no Rust toolchain (`cargo`/`rustc` absent; `static.rust-lang.org` and `crates.io` are unreachable),
- no access to `www.youtube.com` or `googleapis.com`.

Consequences, stated plainly:

1. **No code in this repository has been compiled.** `cargo fmt`, `cargo clippy`, `cargo check` and
   `cargo test` have **never run** here. They are wired into `.github/workflows/ci.yml` and are the
   first place this tree will be built.
2. **No unit test result is reported as passing**, because none was executed. Test code exists and is
   listed below, but it is *unexecuted*.
3. **No upstream request has ever been made.** Anything about live YouTube behaviour is derived from
   reading the reference implementations (`docs/research/`), not from observation.
4. Because of (3), capabilities that depend on unverified encodings are **not declared** by default
   (see "Conservative capability policy").

## Phase status

| Phase | State | Evidence |
|---|---|---|
| 1 — Multi-repository research | complete | 5 documents in `docs/research/`, 12 repositories cloned and read |
| 2 — Architecture design | complete | `ARCHITECTURE.md`, `PROVIDER_INTERFACE.md`, `API_CONTRACTS.md`, 16 ADRs |
| 3 — Rust foundation | written, uncompiled | 12 crates, workspace manifests, CI, Docker, migrations |
| 4 — Provider engine | partially written, uncompiled, unverified | contract, transport, InnerTube search/player/captions, registry, fixtures |
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

1. Run the CI pipeline in an environment with `crates.io`; fix compile errors (expected).
2. Live-verify InnerTube bootstrap (`sw.js_data` + `ytcfg`) and the `params` encoder; only then enable
   `search_filters`/type-scoped search by default and capture real fixtures.
3. Channels + playlists (browse endpoint), then comments (`next` + continuations).
4. Redis cache + quota; OpenTelemetry; worker jobs with a Postgres lease.
5. Benchmarks + load tests; publish measured numbers instead of claims.
