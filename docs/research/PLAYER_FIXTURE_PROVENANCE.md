# Player fixture provenance

Every player fixture under `crates/vydra-providers/tests/fixtures/` with the exact bytes it came from,
what was removed, and what a test using it is **allowed** to claim. The class vocabulary and the
general rules are in `docs/research/FIXTURE_PROVENANCE.md`; this file is the player-specific appendix
that the Phase 5A fixtures are required to have.

Captures were made by Vydra itself: `crates/vydra-providers/examples/probe_player.rs` through the
production `ReqwestTransport` + `InnertubeClient`, from GitHub Actions runners on 2026-11-14. Raw
archives (gzip, with SHA-256 of both the raw and the compressed bytes) live in
`docs/research/data/player-probe-<run>/`.

| Capture run | UTC | Commit | Requests | Status | Archive |
| --- | --- | --- | --- | --- | --- |
| `43549141233` | 2026-11-14T02:59:51Z | `50466cc4` | 4 raw + 3 provider-path | all HTTP 200 | `docs/research/data/player-probe-43549141233/` |
| `43549736295` | 2026-11-14T03:05:36Z | `cf5aa849` | 4 raw + 3 provider-path | all HTTP 200 | `docs/research/data/player-probe-43549736295/` |

Run `43549736295` is the fixture source. Run `43549141233` is kept because it is a second independent
observation of the same verdicts, and because it is the capture in which the `age_restricted`
misclassification is visible in the recorded provider projection. Its redaction pass was stricter in
the second run: run 1 kept the response-level `messages` promotion surface, which is where the last
relative `apiUrl` query strings lived. No key, cookie, signature or session material was present in
either archive.

## 1. Fixtures

| File | Class | Bytes | SHA-256 | Derived from | Used by |
| --- | --- | --- | --- | --- | --- |
| `recorded/player_probe_public_unplayable_2026-11-14.json` | `recorded_vydra_probe` | 12 879 | `99e1a0145e7929e4…` | run `43549736295`, label `public` | `player_parser::live_unplayable_capture_*`, `unknown_future_fields_are_ignored`, `missing_microformat_falls_back_to_video_details_thumbnails` |
| `recorded/player_probe_login_required_2026-11-14.json` | `recorded_vydra_probe` | 3 536 | `7543e02c6468316e…` | same run, label `captions` | `player_parser::live_login_required_capture_is_a_typed_unavailability`, `restricted_answers_are_not_parse_errors` |
| `recorded/player_probe_error_unavailable_2026-11-14.json` | `recorded_vydra_probe` | 628 | `f49b08f0867b505d…` | same run, label `missing` | `player_parser::live_error_capture_is_a_typed_unavailability_without_inventing_a_reason` |
| `malformed/player_probe_without_playability_2026-11-14.json` | `derived_malformed` | 11 662 | see sidecar | `recorded/player_probe_public_unplayable_2026-11-14.json` with `playabilityStatus` removed (keys re-sorted, indentation normalised) | `player_parser::transformed_capture_without_a_verdict_is_unknown` |
| `recorded/player_web_2025-11-10.json` | `recorded_upstream_subset` | 8 963 | — | `yuliskov/MediaServiceCore` @ `b3d561f`, `player/web/2025.11.10_player_regular.json` (MIT) | `recorded_fixture::recorded_player_page_maps_availability_formats_and_captions` |
| `malformed/player_without_playability.json` | `derived_malformed` | 6 652 | — | the 2025 recording with `playabilityStatus` removed | `recorded_fixture::malformed_player_without_playability_warns_but_keeps_the_document` |

The exact digest of every fixture is in its `*.meta.json` sidecar, and
`python3 scripts/fixtures/import_player_capture.py <archive> --check` re-verifies the derived ones
against the archive manifest.

## 2. What was removed from the captures

Applied by `examples/probe_player.rs` before anything was written to disk, then verified by a
self-check that withholds the body on failure:

| Removed | Why |
| --- | --- |
| `responseContext`, `frameworkUpdates`, `trackingParams`, `clickTrackingParams`, `serviceTrackingParams`, `loggingContext`, `sessionIndex`, `delegatedSessionId` | session/tracking material with no evidentiary value |
| `signatureCipher`, `cipher` | signature blobs; Vydra never deciphers and never should |
| response-level `messages`, `adBreakHeartbeatParams` | advertising/promotion surfaces; also the last place a relative `apiUrl` query string survived |
| the query string of every string value that is a URL | that is where `sig`, `n`, `expire`, `ip`, `pot` and the key live. `?` count in the archived bodies: **0** |
| nothing else | values were never rewritten; the fixtures are the archived bytes verbatim |

The api key is a query parameter on the *request*, never in the body or a header; it appears nowhere in
the captures (`AIza` count: 0 in all archived bodies).

## 3. What a test may claim with these fixtures

A `recorded_vydra_probe` fixture supports: "Vydra received this exact document, on this date, from this
network, and its parser does X with it."

It does **not** support:

* "upstream returns this today" — a snapshot, not a contract;
* "this is what a residential IP sees" — the runner IP is bot-checked, and the captures show that
  (`UNPLAYABLE`, `LOGIN_REQUIRED`), which is itself the honest reason the format/caption sections stay
  `UNVERIFIED_LIVE`;
* "the parser handles every player response" — the corpus has no live `OK` verdict, no live
  `streamingData`, no live `captions`, and no age/geo/private/members-only sample. Those branches rest on
  the third-party 2025 recording, on unit tests, or on nothing at all (see
  `docs/reports/PHASE_5A_REPORT.md` §"Verification matrix").

## 4. Regeneration

```bash
# 1. capture (needs egress to www.youtube.com; do not run in the development sandbox)
cargo run --release -p vydra-providers --example probe_player

# 2. archive (raw redacted bytes → gz + manifest with SHA-256)
python3 scripts/fixtures/archive_probe.py probe-out docs/research/data/player-probe-<run> \
    --kind player --capture-run <run>

# 3. fixtures (verbatim bytes for live captures, named transformation for derived ones)
python3 scripts/fixtures/import_player_capture.py docs/research/data/player-probe-<run>

# 4. check, without writing
python3 scripts/fixtures/import_player_capture.py docs/research/data/player-probe-<run> --check
```

Restoring an archived body: `gunzip -c <name>.json.gz > <name>.json` — the manifest records the SHA-256
of both forms.
