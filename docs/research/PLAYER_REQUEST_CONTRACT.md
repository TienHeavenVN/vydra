# Player request contract (`POST /youtubei/v1/player`)

What Vydra sends to the player endpoint, how that shape is enforced in code, and which parts of it are
**verified against upstream** rather than inferred.

Phase: 5A. Evidence: two captures made by Vydra itself on 2026-11-14 (CI runs `43549141233` and
`43549736295`), archived under `docs/research/data/player-probe-43549141233/` and
`docs/research/data/player-probe-43549736295/`. Analysis: `docs/research/PLAYER_RESPONSE_ANALYSIS.md`.
Provenance of the fixtures derived from them: `docs/research/PLAYER_FIXTURE_PROVENANCE.md`.

> A capture proves that upstream **accepted** this request on that date from that network with that
> client profile. It does not prove that every field is *required*, or that removing one would still be
> accepted. Statements below are labelled accordingly.

---

## 1. The request

```
POST https://www.youtube.com/youtubei/v1/player?prettyPrint=false&key=<api key>
content-type: application/json
x-youtube-client-name: 1
x-youtube-client-version: 2.20260805.01.00
origin: https://www.youtube.com
referer: https://www.youtube.com/
accept-language: en,en;q=0.8
user-agent: Mozilla/5.0 (…) Cobalt/Version          ← depends on the client profile
x-goog-visitor-id: <bootstrap visitor id>            ← only when the bootstrap produced one
```

Body, byte-exact as it appears on the wire (keys alphabetical — see the note below):

```json
{"contentCheckOk":true,"context":{"client":{"clientName":"WEB","clientVersion":"2.20260805.01.00","gl":"US","hl":"en","originalUrl":"https://www.youtube.com","platform":"DESKTOP","visitorData":"<visitor>"},"request":{"internalExperimentFlags":[],"useSsl":true},"user":{"lockedSafetyMode":false}},"racyCheckOk":true,"videoId":"9Umm2MAKsMc"}
```

**Wire ordering note (Phase 5A).** The typed body has a fixed struct order, but it is converted to
`serde_json::Value` before encoding (`provider.rs::serialize_body`), and `serde_json::Map` is a
`BTreeMap` unless the `preserve_order` feature is enabled — so the bytes on the wire are key-sorted.
That is deterministic (the same request always produces the same bytes, which is what
`player_body_is_byte_exact` pins) but it is *not* struct order. The earlier wording in this repository
that claimed struct order on the wire was wrong; the test that compares bytes caught it.

| Element | Value | Verified how |
| --- | --- | --- |
| method | `POST` | 4 live requests, all HTTP 200 |
| path | `/youtubei/v1/player` | same |
| query | `prettyPrint=false`, `key=<bootstrap key>` | same; the key is a **query parameter**, never a header |
| `videoId` | the 11-character id from the validated `VideoId` newtype | same; the four captures used three distinct ids, all echoed in `videoDetails.videoId` where metadata was returned |
| `contentCheckOk`, `racyCheckOk` | `true`, `true` | accepted; see §3 for the paired false/false request |
| `context` | one object, built by `ClientContext::new` from the client profile + session | accepted on every request; identical to the search context |
| header `x-youtube-client-name` / `-version` | `1` / bootstrap version | accepted; a mismatch is a plausible cause of `UNPLAYABLE`, so the version is taken from the bootstrap rather than hard-coded |

The type has no other fields: a player request physically cannot carry `query`, `params`,
`continuation` or a cursor, which is enforced by `tests/request_contract.rs` (see §5).

## 2. What is not sent, and why

| Not sent | Reason |
| --- | --- |
| playback parameters (`playbackContext`, `contentPlaybackContext`, `signatureTimestamp`) | Vydra only reads metadata; playback is out of scope (ADR-006) and `signatureTimestamp` would be an arms-race artifact |
| PoToken / BotGuard attestation | out of scope, and circumventing bot detection is explicitly forbidden |
| cookies, `SAPISID`, `Authorization` | Vydra has no accounts; the session is the public web bootstrap only. A consent cookie is sent **only** if `consent_cookie` is configured explicitly |
| `visitorData` in a header *and* a body field with different values | one source: the bootstrap session |
| `params` (filter blob) | search-only, and still unverified there |
| any retry on a non-2xx | a 4xx/5xx is surfaced; retrying is the registry's decision, bounded by `max_attempts` |

## 3. The check flags: what the live capture showed

The provider sends `contentCheckOk: true, racyCheckOk: true` — what the anonymous web client sends.
These flags acknowledge a content warning; they are not an age or account bypass, and the age gate
still answers `LOGIN_REQUIRED`.

To avoid asserting that the flags are irrelevant, the probe sends the **same video twice**, once with
`true/true` and once with `false/false`:

| Run | `true/true` | `false/false` | Result |
| --- | --- | --- | --- |
| `43549141233` | 14 135 B | 14 519 B | both `UNPLAYABLE`, same reason; bodies differ only in the promotion surface that run 1 still kept |
| `43549736295` | 12 879 B redacted | 12 879 B redacted | **byte-identical** after redaction (`sha256 99e1a014…`) |

Conclusion, stated narrowly: for this video and this network, the flags did not change the verdict, the
metadata or the failure mode. The flags stay `true/true` because that is what the web client sends and
the metadata path is Vydra's only interest; a deployment that wants the conservative variant can call
`PlayerRequest::with_checks(..., false, false)` without any other change.

## 4. Where the shape is enforced

| Level | Mechanism |
| --- | --- |
| type | `PlayerRequest` has exactly four fields, `Serialize`/`Deserialize`; deterministic wire bytes (key-sorted, see the note in §1) |
| construction | `InnertubeProvider::video` → `serialize_body(&PlayerRequest::new(context, id))`; no `json!` at the call site |
| validation | `VideoId` is a newtype: an invalid id cannot reach this layer, so the id cannot be used to build an arbitrary URL |
| transport | https-only, host allowlist, `Policy::none()` redirects, 3 s connect timeout, per-request deadline, 4 MiB body cap, ≤32 headers, no IP literals/userinfo/fragments (`docs/security/TRANSPORT_RELIABILITY.md`) |
| secret hygiene | the key lives only in the query string; `tests/request_contract.rs` asserts no header value ever contains `AIza` |

## 5. Tests that pin this contract

`crates/vydra-providers/tests/request_contract.rs`:

| Test | Assertion |
| --- | --- |
| `player_body_is_byte_exact` | the serialized body equals a literal string, byte for byte |
| `player_posts_the_video_id_with_the_check_flags` | endpoint, query string, sorted body keys, flag values |
| `player_request_carries_no_search_only_fields` | no `query`, `params`, `continuation`, `cursor`, `browseId` |
| `player_headers_match_the_web_client_profile` | header set, `accept-language` follows `hl`, no `cookie` unless configured, key only in the query |
| `innertube/request.rs` unit tests | `PlayerRequest` round-trips; `with_checks` serializes the opposite flags verbatim |

## 6. What is still unverified

| Item | Status | What would settle it |
| --- | --- | --- |
| whether each header is *necessary* (vs. merely accepted) | **UNVERIFIED** | remove one header at a time and watch for a verdict change; expensive and low value while the verdict is already restricted |
| `x-youtube-client-name/version` vs. bootstrap version for other clients (`MWEB`, `TV`) | **UNVERIFIED** | one probe run per client profile |
| behaviour on a residential/corporate IP | **UNVERIFIED** | the runner IP is bot-checked; see `PLAYER_RESPONSE_ANALYSIS.md` §5 |
| `contentCheckOk` effect on genuinely age-gated content | **UNVERIFIED** | needs an age-gated id; not attempted, because obtaining one requires either guessing ids or reproducing a restriction Vydra cannot verify |
| other `gl`/`hl` values | **UNVERIFIED** | one run per region; the probe already accepts env overrides |

## 7. Re-verification

The permanent entry point is `.github/workflows/probe.yml` (`workflow_dispatch`, job `player`); it
takes `public_video_id`, `captions_video_id` and `missing_video_id` as inputs and uploads
`player-probe-out` plus a pre-built archive. To commit a capture as evidence:

```bash
cargo run --release -p vydra-providers --example probe_player     # needs egress to www.youtube.com
python3 scripts/fixtures/archive_probe.py probe-out docs/research/data/player-probe-<run> \
    --kind player --capture-run <run>
python3 scripts/fixtures/import_player_capture.py docs/research/data/player-probe-<run>
```

`import_player_capture.py --check` re-verifies that the committed fixtures still match the archived
SHA-256 digests.
