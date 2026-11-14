# Progress and Verification Status

Last updated: 2026-11-14. This file is the authoritative answer to "does it work?".

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
3. **Upstream requests are made only from CI, and only by the probe.** Phase 4.3 changed this: on
   2026-11-14 five `/youtubei/v1/search` requests were sent through the production transport from a
   GitHub-hosted runner (runs `43541388070`, `43542839419`) and reduced into
   `crates/vydra-providers/tests/fixtures/recorded/search_web_probe_*.json`. The sandbox still cannot
   reach YouTube, so everything about live behaviour outside those captures is derived from the
   reference implementations in `docs/research/`.
4. Because of (3), capabilities that depend on unverified encodings are **not declared** by default
   (see "Conservative capability policy").
5. The CI status of any commit is a moving target; the authoritative source is the Actions run for the
   current head of `arena/06c8b7fc-vydra`.

## Phase status

| Phase | State | Evidence |
|---|---|---|
| 1 — Multi-repository research | complete | 5 documents in `docs/research/`, 12 repositories cloned and read |
| 2 — Architecture design | complete | `ARCHITECTURE.md`, `PROVIDER_INTERFACE.md`, `API_CONTRACTS.md`, 19 ADRs (`docs/research/IMPLEMENTATION_DECISIONS.md`) |
| 3 — Rust foundation | compiles in CI | 12 crates, workspace manifests, CI, Docker, migrations |
| 4 — Provider engine | **audited, remediated, verification in CI** | contract, transport, InnerTube search/player/captions, registry, fixtures; 20 findings closed (see `docs/security/PROVIDER_ENGINE_AUDIT.md`) |
| 4.1 — Provider engine audit | **complete, CI green** | `PROVIDER_ENGINE_AUDIT.md` (27 findings, A-01…A-27) + `PROVIDER_ENGINE.md`; every CI job green on run [43448221721](https://github.com/TienHeavenVN/vydra/actions/runs/43448221721), 22/22 after the temporary `fmt-apply` job was retired |
| 4.2 — Upstream request contract verification | **complete, CI green** (https://github.com/TienHeavenVN/vydra/actions/runs/43457728940) | `docs/research/INNERTUBE_REQUEST_CONTRACTS.md`, `docs/research/FIXTURE_PROVENANCE.md`, `docs/security/DNS_REBINDING_ANALYSIS.md`, `docs/reports/PHASE_4_2_REPORT.md`; request-contract + recorded-fixture tests; A-28…A-32 closed |
| 4.3 — Search evidence & reliability | **complete, CI green** (https://github.com/TienHeavenVN/vydra/actions/runs/43545086009, 22/22) | 5 live captures from 3 runs, typed request DTOs, parser + transport reliability suites, `docs/reports/PHASE_4_3_REPORT.md`, `docs/research/SEARCH_RESPONSE_ANALYSIS.md`, `docs/security/TRANSPORT_RELIABILITY.md` |
| 5 — Search engine | written for mixed search only | type-scoped search gated behind an opt-in flag |
| 6 — Metadata engines | video + chapters + captions metadata | channels, playlists, comments **not implemented** |
| 7 — Advanced modules | not started | live, music, trending, related, discovery |
| 8 — Infrastructure | skeleton only | Postgres schema + repository stub; Redis/NATS/OpenSearch deferred (ADR-015) |
| 9 — Hardening | not started | security model documented, not tested |

## Phase 4.3 — search evidence and reliability (what CI actually proved)

| Claim | Evidence |
|---|---|
| Vydra can talk to `/youtubei/v1/search` through its own transport | 5 captures, statuses 200×4 + 400, run [43544223832](https://github.com/TienHeavenVN/vydra/actions/runs/43544223832); earlier runs [43541388070](https://github.com/TienHeavenVN/vydra/actions/runs/43541388070), [43542839419](https://github.com/TienHeavenVN/vydra/actions/runs/43542839419) |
| A search continuation works with the token alone, and pages the *same* query | page 2 with a `{context, continuation}` body: 200, 488 298 B, 18 items, and the same total estimate as page 1 (23 544 805 vs 23 544 604) |
| The page cursor is not a filter chip | the cursor bug and its live before/after comparison are in `SEARCH_RESPONSE_ANALYSIS.md` §2.1 |
| A malformed search body is rejected, not silently emptied | 400 `FAILED_PRECONDITION`, 274 B, captured verbatim |
| Request bodies are typed, not assembled ad hoc | `innertube/request.rs` (7 unit tests) + `tests/request_contract.rs` |
| The parser cannot turn unknown content into an item | `tests/search_parser.rs` (19 tests, 5 of them on the probe recordings) |
| Transport failures are typed and not retried | `tests/transport_reliability.rs` (6 tests) |
| Type-scoped filters are still disabled | `enable_unverified_filters: false`; no `params` on any request the probe sent |

Full detail, including what the captures *do not* prove, is in `docs/reports/PHASE_4_3_REPORT.md`.

## Phase 4.1 — compile stabilisation (what CI actually proved)

Run `43448221721` on commit `524b0d7`: **every job green** — `fmt`, `check`, `clippy`
(`-D warnings`), `test` (14 packages, one job each), `audit`, `deny`, `secrets`, `openapi`,
`docker` (23 jobs including the temporary `fmt-apply`; 22/22 on every run since). Every `cargo`
command behind that result ran on a GitHub runner, not in this sandbox.

The loop took eighteen pushes; each failing run exposed a real defect rather than a CI quirk.
Bugs found by first executing the tests (`docs/security/PROVIDER_ENGINE_AUDIT.md` has the detail):

| Finding | What it meant in production |
| --- | --- |
| A-21 | The metadata cache could never hit — unset optional fields were skipped on write and required on read. |
| A-22 | Every channel handle was rejected, so handle-based channel lookup never worked. |
| A-23 | The session bootstrap discarded the API key it had just downloaded. |
| A-24 | Search results came back in reverse document order. |
| A-25 | The "unknown renderer" counter reported layout containers instead of unmapped content. |
| A-26 | A test fixture that doubled per iteration (2^200 nodes) hung the test job for 20 minutes. |
| A-27 | Unused dependencies at crate and workspace level failed the build under `-D warnings`. |

CI itself needed three fixes to be usable as a compiler of record: `cargo` was run with
`--message-format=short`, so diagnostics never start with `error` and every `grep '^error'`
matched nothing (now `scripts/ci/report.sh`); annotations were emitted from a *separate* step,
which the runner skips when the build step is killed; and a workspace-wide `cargo test` was killed
after ~3 minutes, so the suite now runs as a fail-fast:false matrix of one job per package with a
240 s cap and a per-test probe (`scripts/ci/test-package.sh`) that names a hanging test.

The temporary `fmt-apply` job (which pushed rustfmt commits to the branch) is retired now that
`fmt` has been green for nine consecutive runs.

## Phase 4.2 — request contracts, fixtures, rebinding (what CI actually proved)

Scope: verify what Vydra *sends* and what it *expects back*, before any further engine work. No new
endpoint, no new capability, no signature/PoToken handling, no live request.

| Item | Result |
| --- | --- |
| Request shapes pinned | search `{context, query[, params]}`, continuation `{context, continuation}`, player `{context, videoId, contentCheckOk, racyCheckOk}`, exact URL including `?prettyPrint=false&key=…`, header set, context object — `tests/request_contract.rs` |
| Fixture corpus | 3 recorded subsets (MIT, `yuliskov/MediaServiceCore` @ `b3d561f`), 1 synthetic, 3 derived malformed, each with a `*.meta.json` sidecar; layout `tests/fixtures/{synthetic,recorded,malformed}/` |
| Recorded surface coverage | browse page with search-tab renderers, browse continuation (582-char token), web player response (availability, 4 formats, ASR caption) |
| Still unrecorded | global `/youtubei/v1/search` response, search continuation envelope, current bootstrap document — all marked *inferred* |
| Defects found | A-29 (api key never sent), A-30 (`reqwest` ambient proxy defeated the DNS guard), A-31 (two parse warnings did not count), A-32 (fixture misnamed after `/search`) |
| Security | A-28 DNS rebinding closed (`GuardedResolver`), A-30 closed (`no_proxy()`); residual risks listed in `docs/security/DNS_REBINDING_ANALYSIS.md` §6 |
| `Cargo.lock` | ADR-019 executed: generated in CI, validated with `cargo check --locked`, committed |

What this phase deliberately did **not** establish: that YouTube accepts these requests today, that the
recordings still match upstream, or that any search-continuation works. Those need a live request or a
newer recording, and they are listed as such in every document that touches them.

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
