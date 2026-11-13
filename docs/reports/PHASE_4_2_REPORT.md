# Phase 4.2 — Upstream request contract verification

Branch `arena/06c8b7fc-vydra`, PR [#1](https://github.com/TienHeavenVN/vydra/pull/1). Baseline for
this phase: `157bdb7` (22/22 green, run
[43451265548](https://github.com/TienHeavenVN/vydra/actions/runs/43451265548)). Head of the phase:
`PHASE_HEAD_PLACEHOLDER`.

**The one sentence that governs this report:** Vydra has never sent a request to YouTube. Every
statement below about upstream behaviour is either read out of Vydra's own code, read out of a
reference implementation, or checked against a recording that somebody else captured. Where none of
those exist, the row says "inferred" and stays that way.

---

## 1. Files inspected

| Area | Files |
| --- | --- |
| Request construction | `crates/vydra-providers/src/innertube/{client,provider,mod,params}.rs`, `crates/vydra-providers/src/transport.rs` |
| Response parsing | `crates/vydra-providers/src/innertube/map.rs`, `crates/vydra-providers/src/parse.rs`, `crates/vydra-common/src/text.rs` |
| Call chain | `crates/vydra-api/src/lib.rs`, `crates/vydra-engines/src/lib.rs`, `crates/vydra-provider-registry/src/lib.rs` |
| Domain contract | `crates/vydra-domain/src/{common,video,search,playlist,captions}.rs` |
| Tests and fixtures | `crates/vydra-providers/tests/*.rs`, `tests/fixtures/**`, `scripts/fixtures/extract_recorded.py` |
| Reference implementations | `yuliskov/MediaServiceCore` (`youtubeapi/src/main/java/com/liskovsoft/youtubeapi/{search,innertube,retrofit}/**`, `youtubeapi/src/test/resources/**`), `yt-dlp/yt_dlp/extractor/youtube/{_base,_search}.py`, `LuanRT/YouTube.js` (`src/core/{Session,Actions}.ts`, `src/utils/HTTPClient.ts`, `src/parser/youtube/Search.ts`) |
| Dependencies | `reqwest` 0.12.12 `src/dns/resolve.rs`, `src/async_impl/client.rs` |

Licence positions were checked before any fixture byte was reused: MediaServiceCore, YouTube.js and
`innertube` are MIT, yt-dlp is Unlicense (usable); NewPipeExtractor, rustypipe, ytmusic-rs (GPL-3) and
Piped/Piped-Backend/invidious/youtubei-rs (AGPL-3) were read for behaviour only and contributed
nothing.

## 2. Request structures verified

Full detail, with per-field citations, in `docs/research/INNERTUBE_REQUEST_CONTRACTS.md`.

| Request | Shape now pinned by a test | Evidence class |
| --- | --- | --- |
| bootstrap | `GET https://www.youtube.com/sw.js_data`, parsed for `visitorData` / `clientVersion` / `AIza…` | reference + synthetic |
| search | `POST /youtubei/v1/search?prettyPrint=false&key=…`, body `{context, query}` | reference + synthetic |
| search + filters | body `{context, query, params}` — gated behind `enable_unverified_filters` | reference only |
| continuation | body `{context, continuation}` (no query, no params) | reference only |
| player | `POST /youtubei/v1/player`, body `{context, videoId, contentCheckOk, racyCheckOk}` | reference + recorded response |
| headers | `content-type`, `x-youtube-client-name`, `x-youtube-client-version`, `origin`, `referer`, `accept-language`, `user-agent`, `x-goog-visitor-id`, optional `cookie` | reference |
| context | `client.{hl,gl,clientName,clientVersion,originalUrl,visitorData}`, `user.lockedSafetyMode`, `request.{useSsl,internalExperimentFlags}` | reference |

`tests/request_contract.rs` asserts the exact URL (including the `key=` query parameter), the header
set, and the sorted key set of each body, against a `ReplayTransport`. Passing that test proves what
Vydra *sends*; it proves nothing about how upstream answers.

## 3. Fixture provenance

Full table in `docs/research/FIXTURE_PROVENANCE.md`. Summary:

* `recorded/` — three reduced subsets of captures published by `yuliskov/MediaServiceCore` at commit
  `b3d561f`: a channel *browse* page carrying search-tab renderers (2023-12-06), a browse
  continuation (2023-02-23), and a web *player* response (2025-11-10). MIT, query strings stripped, no
  cookies or tokens retained, values never rewritten, `not_verified` recorded per file.
* `synthetic/` — a hand-written mixed search page, moved out of the flat fixture directory so it can
  never be mistaken for a capture.
* `malformed/` — three derived documents (missing `contents`, missing `playabilityStatus`, truncated
  JSON), each with `derived_from` and `purpose` in its sidecar.
* `scripts/fixtures/extract_recorded.py` regenerates `recorded/` + `malformed/` deterministically from
  a pinned upstream checkout.

Honest limits: no recording of a global `/youtubei/v1/search` response exists in the sources examined,
so search pagination and the search container path remain **inferred**; the only continuation
recording is a browse one, and its token is expired.

## 4. Confirmed defects

| ID | Severity | Defect |
| --- | --- | --- |
| A-29 | S1 | **The api key was never sent.** Bootstrap downloaded it, stored it in `SessionData.api_key`, and every endpoint call went out keyless (`…?prettyPrint=false`, no `key=`). Both reference clients send it (yt-dlp `_base.py:858-866` as a query parameter; MediaServiceCore keeps `Session.apiKey`). |
| A-30 | S2 | **`reqwest` proxy auto-detection defeated the DNS guard.** The builder never called `.no_proxy()`, so an ambient `HTTPS_PROXY` would have made the proxy resolve the target — bypassing `GuardedResolver` entirely. Found while writing the rebinding analysis, which had claimed "no ambient proxy". |
| A-31 | S2 | **Two robustness signals did not count.** A renderer-less search document and a player response without `playabilityStatus` were recorded with `ParseReport::warn`, which does **not** increment `ParseReport::count()` — so `provenance.parse_warnings` stayed `0`, strict mode accepted the document, and the gap was invisible to API callers. They are now `structural_problem` / `missing_field`. |
| A-32 | S3 | **A fixture was named after an endpoint it never saw.** `recorded/search_web_2023-12-06.json` is a channel *browse* capture; the name implied a `/search` response. Renamed to `browse_channel_search_2023-12-06.json` and the test renamed accordingly. |

A-30 and A-31 were found by this phase; A-29 and A-32 are pre-existing.

## 5. Fixes applied

1. `InnertubeClient::endpoint_url` builds `…?prettyPrint=false&key={api_key}`, validates the key with
   the existing `looks_like_api_key` **before** interpolation (a hostile bootstrap document cannot
   inject query parameters), and never puts it in a header or a log line. A keyless session stays
   keyless rather than inventing one.
2. `ReqwestTransport::new()` calls `.no_proxy()`.
3. `no_renderers` → `structural_problem("no_renderers")`; missing playability →
   `missing_field("playabilityStatus", "status")`. Both now reach `parse_warnings`, the public
   `meta` and strict mode.
4. Fixture corpus restructured into `synthetic/`, `recorded/`, `malformed/` with provenance sidecars
   and a README that states the three-class rule; the misnamed recording renamed.
5. New tests: `tests/request_contract.rs` (6), `tests/recorded_fixture.rs` (5), three unit tests for
   `endpoint_url`, one unit test for the missing-playability warning.
6. `Cargo.lock` decision executed (ADR-019 updated): the lockfile is generated in CI, validated with
   `cargo check --locked`, and committed only if that passes — see §6.

## 6. Tests executed

Authority is GitHub Actions only; nothing was compiled in the sandbox.

`CI_EVIDENCE_PLACEHOLDER`

Test inventory added by this phase:

| File | Tests |
| --- | --- |
| `tests/request_contract.rs` | `search_posts_context_and_query_to_the_search_endpoint`, `continuation_posts_only_the_token`, `player_posts_the_video_id_with_the_check_flags`, `bootstrap_is_used_once_and_the_key_never_travels_in_a_header`, `keyless_bootstrap_produces_a_keyless_request`, `empty_search_document_is_a_lenient_warning` |
| `tests/recorded_fixture.rs` | `recorded_browse_page_maps_every_renderer`, `recorded_continuation_page_keeps_the_trailing_token`, `recorded_player_page_maps_availability_formats_and_captions`, `malformed_search_without_contents_is_an_empty_page`, `malformed_player_without_playability_warns_but_keeps_the_document`, `truncated_search_document_is_a_parse_error` |
| `src/innertube/client.rs` (unit) | `endpoint_url_carries_the_session_key_as_a_query_parameter`, `endpoint_url_omits_the_key_when_the_session_has_none`, `endpoint_url_refuses_to_interpolate_a_hostile_key` |
| `src/innertube/map.rs` (unit) | `player_without_playability_records_a_missing_field` |

The 22-job baseline is preserved: no test was deleted or disabled, and `fmt`/`check`/`clippy`/`audit`/
`deny`/`secrets`/`openapi`/`docker` still run. Two temporary jobs (`fmt-apply`, `lockfile`) existed on
this branch only to work around the missing local toolchain and were removed once their output was
committed; their removal is part of the head commit.

## 7. CI run

`CI_URL_PLACEHOLDER`

## 8. Remaining upstream limitations

| Limitation | Consequence | How it stays honest |
| --- | --- | --- |
| No live request was made in this phase | nothing is "verified against YouTube today" | every claim in `INNERTUBE_REQUEST_CONTRACTS.md` carries an evidence class |
| The api key's *validity* is unverified | a wrong key would surface as a `403`/`400` at runtime | the key comes from the bootstrap document; there is no hardcoded literal (A-01) |
| Global search response shape is inferred | the container path may differ from the recorded browse path | leaf-renderer mapping is recorded; `find_continuation` is path-independent |
| Search continuation is unrecorded | pagination shape is inferred | the browse continuation proves token extraction only |
| Recordings are 1–3 years old | shapes may have drifted | `not_verified` in every sidecar; tests state what they do not prove |
| Type-scoped search / filters need `params` | still gated behind `enable_unverified_filters` | unchanged by this phase; capability stays off by default |
| `sig` / `n-sig` / PoToken | not implemented, not attempted | no format carries a URL; `DeliveryKind` reports protection instead |

## 9. Security risks

* **A-28 (closed)**: DNS rebinding is no longer open. `GuardedResolver` implements
  `reqwest::dns::Resolve`, resolves before the socket exists, drops every prohibited address (IPv4,
  IPv6, and IPv4 wrapped in IPv6), fails closed when nothing survives, and hands exactly the surviving
  addresses to the connector — so the validated resolution *is* the connection. TLS hostname
  verification is untouched, and redirects remain refused. Full analysis:
  `docs/security/DNS_REBINDING_ANALYSIS.md`.
* **A-30 (closed)**: ambient proxy support removed, because a proxy resolves the target itself.
* **Residual**: pooled connections are reused without re-resolution (safe, but a blocked address is
  only observed on new connections); a mutually-trusted host returning hostile content is a
  response-side problem outside the transport; `ReplayTransport` (tests) touches no network.
* **No new exposure created**: the api key is a query parameter on an allowlisted origin, not a header
  that could be forwarded to a third party; it is validated for shape; it is never logged.

## 10. Recommended next phase

1. **One recorded `/youtubei/v1/search` first-page response and one search continuation.** This is the
   single highest-value evidence gap: it converts the largest "inferred" block in
   `INNERTUBE_REQUEST_CONTRACTS.md` into "recorded" without touching a network from this sandbox.
2. **Turn the request contract into a schema test** (a serde-derived request DTO asserted against the
   recorded shapes) so a future refactor cannot silently change the wire format.
3. **`params` verification for type-scoped search** before `enable_unverified_filters` is ever turned
   on by default; the current gate is correct and should stay until then.
4. **Proxy support on purpose** (with its own guard) if a deployment needs egress; document the hop.
5. **Timeout/cancellation coverage** at the transport level (a stalled-body test that exercises the
   `tokio::time::timeout` wrapper) — the code exists, the test does not.
