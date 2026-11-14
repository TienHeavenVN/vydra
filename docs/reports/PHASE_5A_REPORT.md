# Phase 5A report — Player endpoint verification

**Result: COMPLETE, with one explicitly unverified area.** `POST /youtubei/v1/player` was called by
Vydra itself, through the production transport and client, and the captures are archived in the
repository. The endpoint, the request shape, the metadata sections and the playability verdicts are
verified against live responses; `streamingData`, `captions` and `storyboards` are **not** verified live,
because upstream refused playback on every capture and Vydra did not attempt to circumvent that.

| Item | Value |
| --- | --- |
| Branch | `arena/06c8b7fc-vydra` (PR #1, left **open and unmerged**) |
| Starting commit | `4b253a1` (Phase 4.3 final, 22/22 green) |
| Final commit | `7816ed0` — the last code/test commit; the docs commit that carries this report follows it (§10) |
| Probe runs | `43549141233`, `43549736295` (both from CI, both archived) |

---

## 1. What was asked, and what was done

| Requirement | Outcome |
| --- | --- |
| Verify the player endpoint independently, using Vydra's own transport | **Done.** `examples/probe_player.rs` uses `ReqwestTransport` + `InnertubeClient`; there is no second HTTP path |
| Capture a minimal set: public video, public video with captions, unavailable/deleted, no age/region bypass | **Done, with a caveat.** Public video captured (`UNPLAYABLE` with full metadata); a second public video returned the bot check; a non-existent id returned `ERROR`. No age/region-restricted id was attempted — obtaining one would have required guessing ids or exercising an access path Vydra does not have |
| Per-attempt record: timestamp, run id, method+path, client, body schema, status, content type, bytes, duration, error class, completeness | **Done.** `summary.json` in each archive, one record per attempt, plus a manifest with SHA-256 of raw and gzipped bytes |
| Raw bodies in `docs/research/data/player-probe-<run>/` with source class, redaction notes, hashes | **Done** (§6) |
| If nothing could be captured: say so, stay BLOCKED, fabricate nothing | Not needed — captures succeeded |
| Inspect before changing; pin the request contract; typed requests | **Done** (§3, `PLAYER_REQUEST_CONTRACT.md`) |
| Analyse the response against the parser; map every playability state; streamingData structure only; no URL extraction | **Done** (`PLAYER_RESPONSE_ANALYSIS.md`) |
| Harden the parser only on evidence | **Done** — six defects, each with a live or reproducible trigger (§5) |
| Typed errors separating transport / HTTP / playability / parse | **Done** (§4) |
| Deterministic tests from archived fixtures | **Done** — `tests/player_parser.rs` (12 tests) + `request_contract.rs` (+3) |
| Docs + PROGRESS + no 4.3 regression + green CI + PR #1 open | **Done** (§8, §9 — run `43552284213`, 22/22) |

## 2. Requests attempted

Four raw `POST /youtubei/v1/player` requests per run against three fixed ids, plus three re-requests
through the provider so the public-API projection is recorded beside the raw bytes. No enumeration, no
retry, no playback request, no cookie or consent flow.

| Label | Video id | Flags | HTTP | Verdict | `videoDetails` |
| --- | --- | --- | --- | --- | --- |
| `public` | `dQw4w9WgXcQ` | `true/true` | 200 | `UNPLAYABLE` — "The page needs to be reloaded." | present |
| `public_no_checks` | `dQw4w9WgXcQ` | `false/false` | 200 | identical verdict; redacted bodies byte-identical | present |
| `captions` | `9Umm2MAKsMc` | `true/true` | 200 | `LOGIN_REQUIRED` — "Sign in to confirm you're not a bot" | absent |
| `missing` | `AAAAAAAAAAA` | `true/true` | 200 | `ERROR` — "This video is unavailable" | absent |

Both runs, six minutes apart, produced the same verdicts. Status distribution over the eight raw
requests: **8× HTTP 200**; verdicts `UNPLAYABLE ×4`, `LOGIN_REQUIRED ×2`, `ERROR ×2`.

**Interpretation.** The runner's datacenter IP is bot-checked by the player endpoint: metadata is served
for one public video, playback is refused for both, and one video is refused entirely. That is an
upstream access decision, recorded as-is. Vydra did not retry, did not add a consent/cookie flow and did
not attempt to defeat the check.

## 3. Confirmed request contract

Verified live (accepted, 8/8 attempts) and pinned by `tests/request_contract.rs`:

```
POST https://www.youtube.com/youtubei/v1/player?prettyPrint=false&key=<bootstrap key>
{"context":{...},"videoId":"<id>","contentCheckOk":true,"racyCheckOk":true}
x-youtube-client-name: 1        x-youtube-client-version: 2.20260805.01.00
origin: https://www.youtube.com  referer: https://www.youtube.com/
accept-language: en,en;q=0.8     x-goog-visitor-id: <bootstrap visitor id>
```

* The key is a **query parameter**, never a header (`request_contract` asserts no header contains `AIza`).
* The body has exactly four members; `player_body_is_byte_exact` pins the bytes. Wire order is
  key-sorted (`serde_json::Value` is a `BTreeMap` here), which is deterministic but is *not* struct
  order — see §5 defect 8.
* No search-only field (`query`, `params`, `continuation`, cursor) can appear — the type has no such
  field, and a test asserts it.
* `contentCheckOk`/`racyCheckOk` were sent both ways: `true/true` and `false/false` produced
  byte-identical redacted responses for the same video, so the flags did not influence the verdict for
  this case. They stay `true/true` (web-client behaviour); the alternative remains one constructor call.
* Full field-by-field detail, including what is *not* sent and why: `docs/research/PLAYER_REQUEST_CONTRACT.md`.

## 4. Confirmed response fields and error taxonomy

Live-confirmed (present and mapped from real bytes): `playabilityStatus.{status,reason,errorScreen}`,
`videoDetails.{videoId,title,author,channelId,lengthSeconds,viewCount,keywords,shortDescription,thumbnail}`,
`microformat.playerMicroformatRenderer.{publishDate,category,ownerChannelName,thumbnail}`, plus the
presence of `responseContext`/`frameworkUpdates`/`trackingParams`/`messages`, which are dropped.

Not observed live (fixture-only): `streamingData`, `captions`, `storyboards`, `playabilityStatus.messages`.

Mapping (ADR-020). **The verdict is a value; "is there a document" is a separate question.**

| Upstream | Outcome | HTTP |
| --- | --- | --- |
| `status: OK` + `videoDetails` | document, `availability.state = "available"` | 200 |
| not OK + `videoDetails` | document, `availability = {state:"unavailable", reason:<mapped>}`, counted message `playability:<STATUS>` | 200 |
| not OK + no `videoDetails` | `ProviderError::Unavailable { reason, message }` (raw status kept in the message, `tracing` warning, 409 `content_unavailable`, never fallback-eligible) | 409 |
| no `playabilityStatus` / no `status` | document, `availability.state = "unknown"` + `missing_field:playabilityStatus.status`; strict mode → `provider_parse` | 200 |
| unknown/future status | never `available`: `reason: other` with the raw status in the message | 200/409 |

Reason selection is whole-word/phrase over `reason`, `playabilityStatus.messages[*].text` and
`errorScreen` text: `private`, `members_only`, `geo_restricted`, `age_restricted`,
`copyright_blocked`, `drm_protected`, `deleted`, `not_broadcasting`, `login_required`, `other`.
`ERROR` + "This video is unavailable" deliberately maps to `other`: upstream does not say whether the
video was deleted or never existed, and inventing `deleted` would be fabrication.

## 5. Parser defects found and fixed

Every one was found in a live capture or in a reproducible transformation of one.

| # | Defect | Found by | Fix |
| --- | --- | --- | --- |
| 1 | `"The page needs to be reloaded."` classified as `age_restricted` (substring `"age"` inside `"page"`) | live provider projection in run `43549141233` | whole-word/phrase matching (`map.rs::mentions`) + regression test with the real string |
| 2 | A restricted answer without `videoDetails` reported as `provider_parse` (a parser bug) instead of a content fact | live `LOGIN_REQUIRED` and `ERROR` captures | verdict classified before `videoDetails` is required; typed `ProviderError::Unavailable` |
| 3 | Missing `playabilityStatus` defaulted to `Available` — a cached, served "playable" document upstream never claimed | transformed capture | new `Availability::Unknown`, never cached, never playable |
| 4 | `playabilityStatus.messages` / `errorScreen` ignored, so the age gate's wording was invisible | live `errorScreen` renderers (`playerErrorMessageRenderer`, `playerInterstitialRenderer`) | used for classification only; never copied into a document |
| 5 | `videoDetails.thumbnail` ignored when `microformat` absent → 0 thumbnails for a real document | live capture has 5 thumbnail sizes in `videoDetails`, 1 in the microformat | fallback to `videoDetails.thumbnail` |
| 6 | Missing `videoDetails.videoId` accepted; the requested id was used instead | code review against the capture set | identity is required; a missing id is a structured parse failure |
| 7 | `publishDate` kept the publisher's offset (`…-07:00`) even though ADR-016 says timestamps are RFC 3339 UTC, so one instant had two wire forms | live capture's `publishDate` asserted in `player_parser` | normalised with `to_offset(UtcOffset::UTC)`; the fixture test asserts `2009-10-25T06:57:33Z` and `offset() == UTC` |
| 8 | Documentation claimed the request body was serialized in struct order; the bytes are key-sorted | `player_body_is_byte_exact` failing on the first run, with both strings in the annotation | test now pins the real bytes; `request.rs`, `INNERTUBE_REQUEST_CONTRACTS.md` and this phase's contract doc corrected |

Public-API consequence of 3: `Availability` gained a variant. Additive, serialization-stable
(`{"state":"unknown"}`), documented in `API_CONTRACTS.md` §1 and covered by a domain test. No other
public type changed; `VideoDocument`, `VideoSummary`, `CaptionTrack`, `MediaFormatInfo` are unchanged.

## 6. Artifact inventory

Two archives, each with `manifest.json` (request records, provider projection, redaction notes, SHA-256
of raw **and** gzipped bytes). `gunzip -c <name>.gz > <name>` restores the original bytes.

Run `43549736295` (fixture source):

| File | Raw bytes | gz bytes | SHA-256 (gz) |
| --- | --- | --- | --- |
| `player_public.json.gz` | 12 879 | 3 161 | `29ad25db7ac045ae311a230ded6f271858ca149257c74ba823737e9f00ac6119` |
| `player_public_no_checks.json.gz` | 12 879 | 3 161 | `29ad25db7ac045ae311a230ded6f271858ca149257c74ba823737e9f00ac6119` |
| `player_captions.json.gz` | 3 536 | 823 | `1e01753efcf3ee1ccae46021456ab68e0505c368ebee5133448decaf30e1ab9c` |
| `player_missing.json.gz` | 628 | 273 | `779d293833186a1751e0ec6c6f79802c88c67ae91bb87b6122e2c8c63d91b579` |
| `summary.json.gz` | 8 313 | 1 516 | `9ecd8ce83df5b2a67e2d46a216e867e8cbd41c5fb5152cb5c6db919a9f71bb46` |

Run `43549141233` (second observation; stricter redaction landed after it, so its bodies still contain
the response-level promotion surface):

| File | Raw bytes | gz bytes | SHA-256 (gz) |
| --- | --- | --- | --- |
| `player_public.json.gz` | 17 650 | 4 349 | `261d9465989742e8a8da39ec2ec6f403e5ab391f6bcab7fa99b4b9cb84fb27d8` |
| `player_public_no_checks.json.gz` | 17 015 | 4 202 | `0c09a94aa5d80107cdffac40da7a79e1c5eff29369f981c1c21fcf88be9e1c21` |
| `player_captions.json.gz` | 8 443 | 2 059 | `cb844ef5f7f19a4ea0c63e941764d015dd8763b4481cbfa79c45977247dd2bc2` |
| `player_missing.json.gz` | 628 | 273 | `779d293833186a1751e0ec6c6f79802c88c67ae91bb87b6122e2c8c63d91b579` |
| `summary.json.gz` | 8 248 | 1 498 | `7e0608e3ebf587afaa8d5743452d1a16b20c8c9ffd80fe01f3a46164d6f0f526` |

Redaction self-check on both archives: `AIza` 0 occurrences, `?` (any query string) 0 occurrences in
run 2, `signatureCipher`/`cipher` absent. Fixtures and their provenance:
`docs/research/PLAYER_FIXTURE_PROVENANCE.md`.

## 7. Verification matrix

Status vocabulary: `VERIFIED_LIVE` (Vydra itself received it), `VERIFIED_FIXTURE`,
`VERIFIED_SYNTHETIC`, `UNVERIFIED`, `BLOCKED`, `NOT_IMPLEMENTED`.

| Capability / behaviour | Status | Basis |
| --- | --- | --- |
| endpoint, method, query, key placement | **VERIFIED_LIVE** | 8/8 accepted |
| request body shape (`videoId`, `contentCheckOk`, `racyCheckOk`, `context`) | **VERIFIED_LIVE** | accepted with exactly these members; byte-exact test |
| client headers (`x-youtube-client-*`, origin, referer, UA, language) | **VERIFIED_LIVE (sufficient, not proven necessary)** | requests succeeded; no ablation performed |
| session bootstrap (`sw.js_data` → key + visitor id) | **VERIFIED_LIVE (indirect)** | every run produced key + visitor id and the player request was accepted |
| `playabilityStatus.status` / `reason` | **VERIFIED_LIVE** | 3 distinct statuses |
| `errorScreen` (two renderers) | **VERIFIED_LIVE** | `playerErrorMessageRenderer`, `playerInterstitialRenderer` |
| `videoDetails` metadata (title, duration, views, channel, author, keywords, description, thumbnails) | **VERIFIED_LIVE** | mapped values asserted from the capture |
| `microformat` (publishDate, category, owner, thumbnails) | **VERIFIED_LIVE** | same |
| availability/`UNPLAYABLE` + metadata → 200 with `unavailable` state | **VERIFIED_LIVE** | capture + fixture test |
| `LOGIN_REQUIRED` / `ERROR` without metadata → 409 `content_unavailable` | **VERIFIED_LIVE** | captures + fixture tests |
| missing verdict → `Availability::Unknown` | **VERIFIED_FIXTURE** (transformed from a live capture) | no live sample without `playabilityStatus` |
| private / members-only / geo / age / copyright / deleted / DRM / premiere wording → reason | **VERIFIED_SYNTHETIC** (unit tests) | no live sample of any of them; wording only |
| `streamingData` (format metadata) | **UNVERIFIED_LIVE / VERIFIED_FIXTURE** | third-party 2025 recording only |
| `captions` (track metadata, asr vs manual, translations) | **UNVERIFIED_LIVE / VERIFIED_FIXTURE** | same |
| `storyboards` | **NOT_IMPLEMENTED** | no parser consumes the section |
| chapters from a real description | **VERIFIED_LIVE (input) / VERIFIED_FIXTURE (markers)** | the capture carries a real 2 375-char description, but it contains no chapter timestamps |
| transport: deadline, cancellation, size cap, redirect not followed, DNS guard, proxy, no secrets in logs | **VERIFIED_FIXTURE** | `transport_reliability.rs`, `player_parser::{http_500_is_an_upstream_error, redirect_is_surfaced_not_followed, truncated_json_is_a_parse_error}` |
| playback URLs / signature deciphering / PoToken | **NOT_IMPLEMENTED (by policy)** | ADR-006; nothing extracted, nothing requested |

## 8. Tests

New and changed this phase:

| File | Tests | Note |
| --- | --- | --- |
| `tests/player_parser.rs` | 12 | live captures + transport/envelope failures |
| `tests/request_contract.rs` | 9 (+3) | byte-exact player body, no search fields, header set |
| `src/innertube/map.rs` (unit) | 16 (+7) | playability classification, identity, thumbnails fallback |
| `src/innertube/request.rs` (unit) | 8 (+1) | `with_checks` serialization |
| `src/vydra-domain/common.rs` (unit) | 4 (+1) | `Availability::Unknown` wire form |
| `tests/recorded_fixture.rs` | 6 | updated: missing verdict is `Unknown`, not `Available` |

No test was deleted or weakened. Two assertions changed because the behaviour they pinned was wrong
(`Available` for a missing verdict; the `Availability::Unavailable` document for a restricted response
with no metadata) and both changes make the assertion stricter.

Provider crate total: **127** `#[test]`/`#[tokio::test]` items; the rewritten 4.3 search suite
(`search_parser` 20, `search_fixture` 3, `recorded_fixture` 6, `transport_reliability` 6) is unchanged
in scope and still green.

## 9. CI

| Run | Purpose | Result |
| --- | --- | --- |
| `43548334088` | compile/format the first probe version (no capture) | red: fmt, clippy (`dead_code` in the probe) |
| `43548781249` | first capture attempt | red: the capture step succeeded, the publish push failed (exit 128) |
| `43549141233` | capture run 1 + publish | capture published (`8e28f1c`), fmt/clippy red on the pre-format commit |
| `43549736295` | capture run 2 + publish | capture published (`10e4ffe`), fmt/clippy red on `cf5aa84` (formatting + `collapsible_match`) |
| `43551388841` | push of the first complete player commit | red: 21/22, `player_body_is_byte_exact` expected struct order instead of the key-sorted wire bytes |
| `43551832624` / `43551835539` | byte-exact body fix | red: 21/22, one header assertion (`accept-language`) — the locale is taken from `ProviderContext`, the test set it on the request only |
| **`43552284213`** (push) / **`43552287221`** (PR) | final commit `7816ed0` | **22/22 green**, no `fmt-apply`, no capture job |

The temporary capture job in `ci.yml` existed only to move bytes from a runner into the repository — CI
artifacts are not downloadable from the development sandbox and expire. It has been removed; the
permanent entry point is `.github/workflows/probe.yml` (`workflow_dispatch`, job `player`, artifact
upload, no push, no required check).

## 10. Files changed

Source: `crates/vydra-providers/src/innertube/{map.rs,request.rs,client.rs}`,
`crates/vydra-domain/src/common.rs`.
Probe/scripts: `crates/vydra-providers/examples/probe_player.rs` (new),
`scripts/fixtures/archive_probe.py` (new), `scripts/fixtures/import_player_capture.py` (new).
Tests: `tests/player_parser.rs` (new), `tests/request_contract.rs`, `tests/recorded_fixture.rs`,
fixtures `recorded/player_probe_*` (3 + sidecars), `malformed/player_probe_without_playability_*`.
Evidence: `docs/research/data/player-probe-43549141233/`, `docs/research/data/player-probe-43549736295/`.
Docs: this report, `PLAYER_RESPONSE_ANALYSIS.md`, `PLAYER_REQUEST_CONTRACT.md`,
`PLAYER_FIXTURE_PROVENANCE.md`, `PROGRESS.md`, `docs/architecture/{PROVIDER_ENGINE,API_CONTRACTS}.md`,
`docs/research/{INNERTUBE_REQUEST_CONTRACTS,IMPLEMENTATION_DECISIONS}.md`,
`docs/security/PROVIDER_ENGINE_AUDIT.md`, `crates/vydra-providers/tests/fixtures/README.md`.
CI: `.github/workflows/{ci.yml,probe.yml}`.

## 11. Remaining limitations and risks

1. **The live verdicts are restricted.** No live `OK` player response was obtained: the runner IP is
   bot-checked. Format and caption metadata remain fixture-verified only. A residential capture (or a
   different runner) would close this; nothing in the code needs to change.
2. **One client profile, one region, one time window.** `WEB`, `hl=en`, `gl=US`, two runs six minutes
   apart. `MWEB`/`TV` and other regions are untested.
3. **Wording-based classification is heuristic.** A new upstream phrasing maps to `other` and is
   surfaced (status in the message, `playability:<STATUS>` message) rather than guessed. That is the
   intended failure mode, but it means the taxonomy can lag upstream wording.
4. **`Availability` gained a variant.** Additive for JSON clients, breaking only for a Rust match with no
   wildcard — none exists in this workspace. Documented in `API_CONTRACTS.md`.
5. **`microformat.likeCount` is present upstream and unmapped** (`like_count` stays `None`). Out of scope
   for this phase; noted so it is a decision, not an oversight.
6. **The probe is manual.** It is not a required check and never will be: a blocked runner IP is a fact
   about the runner, not a regression in Vydra.
7. **No live age/geo/private sample.** Those reasons rest on unit tests over the exact strings; the
   alternative would be fabricating fixtures.

## 12. Next recommendation

Phase 5B should not start with more player probing; it should start with the two things this phase
proved are cheap and missing:

1. **One capture with an `OK` verdict** from a non-datacenter egress (a maintainer's machine, a
   self-hosted runner), which would upgrade `streamingData` and `captions` from fixture-verified to
   live-verified and exercise `media_format_from` against current bytes.
2. **A `gl`/`hl` matrix** using the existing probe inputs (`VYDRA_PROBE_VIDEO_*`, `hl`, `gl`), before any
   new endpoint is added — localization is a parser risk, not a feature request.

After that, the natural next endpoint is `/youtubei/v1/next` (related videos), which shares the player's
page-shape problems and is currently `capability_unavailable`.
