# Phase 4.3 report — search provider evidence and reliability

Scope: the **existing** InnerTube search provider only. No new engine, no Channels/Playlists/Comments
work, no enabled filters, no weakened transport boundary. This report names every file, every fixture,
every test and every claim's evidence, and says plainly what is still unproven.

## 1. Live evidence — what exists now

Vydra made its first real requests to YouTube in this phase, from a GitHub-hosted runner (the
development sandbox has no egress to `youtube.com`), through `ReqwestTransport` with the full guard
active. Nothing was hand-edited afterwards; the reduction is a projection.

| Run | Commit | Purpose | Result |
|---|---|---|---|
| [43541388070](https://github.com/TienHeavenVN/vydra/actions/runs/43541388070) | `a5982e7` | first capture (5 requests) | probe job success; all statuses recorded |
| [43542839419](https://github.com/TienHeavenVN/vydra/actions/runs/43542839419) | `9665091` | capture the fixtures are reduced from, plus publishing the raw output as a commit | probe + publish success; `fmt`/`test` failures unrelated to capture, fixed later |

Captures of run `43542839419` (all `POST /youtubei/v1/search?prettyPrint=false&key=<redacted>`,
`clientName WEB`, `clientVersion 2.20260805.01.00`, `hl=en`, `gl=US`):

| label | request body keys | status | bytes | elapsed | items mapped |
|---|---|---|---|---|---|
| `search_page_1` | `context`, `query` | 200 | 664 211 | 638 ms | 19 (18 video + 1 channel); cursor 638 |
| `search_page_2` | `context`, `continuation` | 200 | 263 144 | 320 ms | 18 video; cursor 1 320 |
| `search_mixed` | `context`, `query` (`veritasium`) | 200 | 1 396 201 | 688 ms | 30 (29 video + 1 channel); cursor 612 |
| `search_empty` | `context`, `query` (nonsense) | 200 | 21 748 | 1 328 ms | 0 |
| `search_error` | `context` only | 400 | 274 | 18 ms | 0 (`FAILED_PRECONDITION`) |

Two conclusions that only a live request could settle:

* **A token-only continuation body is accepted.** `{context, continuation}` → 200 with a complete
  second page and a further token. The typed `ContinuationRequest` (no `query`, no `params` field) is
  therefore not just reference-backed any more.
* **A page has two kinds of token, and Vydra was returning the wrong one** (F-11 below). The cursor it
  extracted for page 1 was a 812-char filter-chip token; the page cursor is 638 chars.
* **A malformed search body is a typed error, not an empty page.** 400 with
  `{"error":{"code":400,"status":"FAILED_PRECONDITION",…}}`; the provider maps it to
  `provider_upstream`, so "no results" and "bad request" cannot be confused.

Analysis of what those documents contain (envelopes, renderer inventory, `estimatedResults` semantics,
the promo page, localization) is `docs/research/SEARCH_RESPONSE_ANALYSIS.md`.

## 2. Capture tooling

| File | Role |
|---|---|
| `crates/vydra-providers/examples/probe_search.rs` | the only sanctioned recording path: typed bodies, production transport, in-process redaction (api key, visitor id, cookies, URL query strings, `webPageUrl`/`canonicalBaseUrl`), emits `search_page_1|2|mixed|empty|error.json` + `summary.json` |
| `crates/vydra-providers/src/innertube/client.rs::post_raw` | returns status + body without mapping status to an error, so an error *document* can be captured instead of an `Err` |
| `.github/workflows/probe.yml` | permanent manual entry point (`workflow_dispatch`) |
| `.github/workflows/ci.yml` job `probe` | **temporary** `[probe]`-gated job used while `probe.yml` could not be dispatched from the branch, plus publishing the raw output as a commit (artifact storage is unreachable from the sandbox) — removed in this change |
| `scripts/ci/summarise-probe.py` | turns `summary.json` into check-run annotations, so the facts survive a failed publish |
| `scripts/fixtures/import_probe.py` | reduces a capture into fixtures; refuses to write if the renderer counts disagree with what the probe reported |
| `scripts/fixtures/inventory_renderers.py` | renders the per-key inventory of a capture (§3 of the analysis document) |

Raw capture (`probe-out/`, ~5.7 MB) was published to the branch only to get it out of CI, reduced into
fixtures, and then deleted from the tree in the same change.

## 3. Typed request DTOs

`crates/vydra-providers/src/innertube/request.rs` (new, 7 unit tests):

* `ClientInfo` / `UserContext` / `RequestContext` / `ClientContext` — one constructor
  (`ClientContext::new(&ClientProfile, &SessionData, hl, gl)`), fixed member order, `visitorData`
  omitted when empty, `clientVersion` falling back to the web profile version, `platform: "DESKTOP"`
  from the profile.
* `SearchRequest` — `{context, query[, params]}`; `params` is private, `None` by default, skipped when
  absent. A request without filters cannot accidentally serialize one.
* `ContinuationRequest` — `{context, continuation}`; **structurally cannot** carry `query` or
  `params`. The unit tests assert the exact serialized bytes.
* `PlayerRequest` — `{context, videoId, contentCheckOk, racyCheckOk}`.

`provider.rs` builds bodies with these types (`serialize_body` → `ProviderError::InvalidRequest` on
serialization failure) and `client.rs::context` is the only producer of the `context` object.
`tests/request_contract.rs` keeps its endpoint/query-string/header assertions on top.

## 4. Parser reliability

The audit produced four changes, each backed by the captures:

| Finding | Evidence | Fix |
|---|---|---|
| **F-1** Shorts shelf items were dropped with no diagnostic: the keys are `shortsLockupViewModel` (not `*Renderer`) inside a `gridShelfViewModel` container | 25 items on page 1, 65 on the mixed page | `UNMAPPED_CONTENT_VIEW_MODELS` (`parse.rs:70`) → counted as `unmapped_content`, never mapped |
| **F-2** `lockupViewModel` is in `KNOWN_RENDERERS` but has no mapping arm, so real items were counted as *chrome* | 11 on page 1, 1 on the continuation page | mapper fallback now calls `ParseReport::unmapped_result` (`map.rs`, `search_item_from_renderer`) |
| **F-3** `continuationItemRenderer` (a token carrier) was counted as unknown content on every paginated page | 1 per page in all captures | added to `STRUCTURAL_RENDERERS` (`parse.rs:40`) |
| **F-4** `playlistRenderer.item_count` read `videoCount` only | no playlist in any capture; reference shape carries both `videoCount` and `videoCountText` | both are read (`map.rs`), labelled reference-backed in the comment |
| **F-11** `find_continuation` returned whichever `continuationCommand` token came first, which on a real page is a **filter-chip** token (812 chars) rather than the page cursor (638) — so `next_cursor` would have silently paged a *filtered* result set while filters are meant to be disabled | page 1 tokens: 1 cursor (638) + 5 chips (746/1 018/754/810/812); the probe recorded 812 for page 1 and 732 for page 2 (whose cursor is 1 320) | `find_continuation` prefers the `continuationItemRenderer` cursor and falls back only when none exists (`parse.rs:374`); `a_filter_chip_token_never_becomes_the_page_cursor` guards it |

Additions that are defensive rather than capture-driven, and are labelled as such in code:

* **Duplicates** — the same entity appearing twice on a page now collapses to its first occurrence and
  is counted as `duplicate_item` (`map.rs::item_key`). No capture contained a duplicate; the rule
  exists so ordering and `limit` arithmetic stay sane. `recorded_first_page_…` asserts none appeared.
* **Three counters** (`unknown_renderer` for chrome, `unmapped_result` for lost results,
  `skipped_item`/`missing_field`/`structural` for unusable ones) — `parse.rs::ParseReport`.

Known limitation, documented and **not** changed: `Strict` mode fails on any warning, and a live page
always carries chrome (133 occurrences on page 1), so `Strict` is a fixture tool. `content_problems()`
already exposes the number that distinguishes "a result was lost" from "the page had buttons"; moving
`Strict` onto it is a Phase 4.4 decision (`parse.rs::check`, `PROVIDER_ENGINE.md` §6a).

## 5. Transport reliability

`crates/vydra-providers/tests/transport_reliability.rs` (6 tests), all with a scripted transport so no
socket is opened:

| Test | Property |
|---|---|
| `an_expired_deadline_never_reaches_the_network` | no request is issued for an expired context (`provider_timeout`, 0 calls) |
| `a_cancelled_future_drops_the_in_flight_request` | dropping the future releases an in-flight request (asserts the call was already issued) |
| `a_transport_timeout_is_reported_as_a_provider_timeout` | `Timeout` → `provider_timeout` |
| `an_oversized_body_is_reported_as_a_parse_error_naming_the_cap` | `BodyTooLarge` → `provider_parse`, message names the cap |
| `a_connection_failure_is_wrapped_and_not_retried` | `Connect` → `provider_transport`, exactly one call (no hidden retry) |
| `a_redirect_is_surfaced_and_never_followed` | 302 → `provider_upstream`, no follow-up call |

`docs/security/TRANSPORT_RELIABILITY.md` records the boundary as configured (https-only, redirects
refused, `no_proxy()`, guarded resolver, caps, timeouts), the failure→code table, and — explicitly —
what is **not** covered: connection reuse across requests, DNS TTL behaviour, rebinding against a live
resolver, and proxies (refused by design; a deployment that needs one must implement a `Transport`
with its own guard). No security control was relaxed to make a test pass.

## 6. Fixtures

`crates/vydra-providers/tests/fixtures/recorded/` — class `recorded_vydra_probe`, one `.meta.json`
each, provenance in `docs/research/FIXTURE_PROVENANCE.md`:

| Fixture | Bytes | Content kept |
|---|---|---|
| `search_web_probe_first_page.json` | 208 616 | 18 video, 1 channel, 25 Shorts placeholders, 812-char token, `estimatedResults` 23 544 834 |
| `search_web_probe_continuation.json` | 230 854 | 18 video, 1 lockup placeholder, 732-char token, `estimatedResults` 19 |
| `search_web_probe_mixed.json` | 452 077 | 29 video, 1 channel, 65 Shorts placeholders, 818-char token |
| `search_web_probe_empty.json` | 525 | no renderers, `estimatedResults` 0 |
| `search_web_probe_malformed_request.json` | 274 | the 400 error document verbatim |

Each sidecar records: run URL, capture time, request body keys, client identity, HTTP status/bytes/ms,
the reduction bullets, the dropped-renderer inventory, and `probe_observation` (what the probe saw on
the unreduced capture), so a projection cannot be mistaken for the live numbers.

## 7. Tests added or changed

| File | Count | Subject |
|---|---|---|
| `crates/vydra-providers/tests/search_parser.rs` | 20 | renderer mapping, skip-not-coerce rules, transparent containers, unknown content, duplicates, localized counts/durations, thumbnails, both continuation shapes, and 5 tests pinned to the probe captures |
| `crates/vydra-providers/tests/transport_reliability.rs` | 6 | §5 |
| `crates/vydra-providers/src/innertube/request.rs` | 7 | exact serialized request bytes |
| `crates/vydra-providers/src/parse.rs` / `map.rs` / `transport.rs` | 8 / 8 / 18 | existing suites, extended for the new counters and the dedup |

No test was deleted or weakened. `search_parser.rs` replaces the pre-existing fixture expectation for
the first page, which asserted "no unknown renderers"; with the real document that assertion was
false in a way that hid F-1 and F-2.

## 8. Deliberately not done

* **Type-scoped filters** stay disabled (`enable_unverified_filters: false`). The `params` protobuf
  encoding remains inferred; no filtered request was sent.
* **Shorts/lockup mapping.** The items are counted and named, not mapped: deriving a video id from
  `entityId` (`shorts-shelf-item-<id>`) is the exact "lookalike id becomes a valid result" failure
  this phase exists to prevent. It needs a capture of the item's reel endpoint.
* **Player live verification.** The probe only calls `/search`; `/player` remains recorded-2025-11-10.
* **Channels, playlists, comments, other engines.** Untouched.
* **Strict-mode change.** Documented, not implemented (§4).

## 9. CI status

*(filled in at the end of this change — see the pull request checks for the authoritative run.)*

| Run | Commit | Result |
|---|---|---|
| 43541388070 | `a5982e7` | probe success; `fmt` red (temporary job, expected) |
| 43542839419 | `9665091` | probe + publish success; `fmt` red (bot fixed in `c73a4e7`), `test (vydra-providers)` red on one parser expectation (fixed) |
| *final* | *this change* | *22 jobs* |

## 10. Remaining risks

1. **One network, one region, one language, one day.** The captures are `gl=US`, `hl=en`,
   2026-11-14. A different region may serve a different tree; the fixtures pin the parser, not
   upstream.
2. **Results Vydra cannot show.** 25/65 Shorts items and 11 lockup cards per page are counted but not
   mapped — callers get fewer results than the page contains, and `parse_warnings` is the only signal.
3. **`params` and filters.** Still inferred, still disabled; a filtered request could behave
   differently from the one capture.
4. **Continuation depth.** One hop verified, and it was a chip continuation (F-11). A true page-2
   capture with the corrected cursor has not been made; tokens expire, and nothing proves page 3+.
5. **Pooled connections** bypass the resolver on reuse (bounded by a 90 s idle timeout) — analysed in
   `docs/security/TRANSPORT_RELIABILITY.md` §4.
6. **No proxy support**, by decision; a deployment behind a mandatory egress proxy cannot use the
   provider as-is.
7. **Capture path is CI-only.** Reproducing the evidence requires a GitHub runner; the sandbox cannot
   re-run it, so a future capture is a new run, not a local command.
