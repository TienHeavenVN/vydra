# Video metadata fixture provenance (Phase 5B)

Which bytes back the video-metadata tests, which class each file belongs to, and what each one proves.
This document extends `docs/research/FIXTURE_PROVENANCE.md` (corpus-wide rules) and
`docs/research/PLAYER_FIXTURE_PROVENANCE.md` (Phase 5A player captures) with the fixtures Phase 5B adds
and the sections it newly reads.

Machine-enforced rules live in `crates/vydra-providers/tests/fixture_provenance.rs`: every fixture needs a
sidecar, the sidecar's `class` must be from the closed set, the directory must agree with the class, and a
synthetic file must say it is not a capture and list what it does not prove.

---

## 1. Fixtures used by the Phase 5B tests

| Fixture | Class | Origin | Used for |
| --- | --- | --- | --- |
| `recorded/player_probe_public_unplayable_2026-11-14.json` | `recorded_vydra_probe` | Vydra's own `POST /youtubei/v1/player`, CI run `43549736295`, 2026-11-14T03:05:36Z, id `dQw4w9WgXcQ`, reduced from the archived raw response | live-verified metadata: title, duration, views, channel, keywords, description, thumbnails, **and the Phase 5B facts** `likeCount`, `publishDate`/`uploadDate` with a `-07:00` offset, `isUnlisted`, `isFamilySafe`, and the `213` vs `214` duration disagreement |
| `recorded/player_web_2025-11-10.json` | `recorded_upstream_subset` | `yuliskov/MediaServiceCore` @ `b3d561f`, MIT, recorded by upstream on 2025-11-10 | the `OK` path: `streamingData` (4 formats), `captions` (1 ASR track), microformat category/publishDate |
| `recorded/player_probe_login_required_2026-11-14.json` | `recorded_vydra_probe` | same run, id `9Umm2MAKsMc` | refusal without metadata → `409 content_unavailable{login_required}` |
| `recorded/player_probe_error_unavailable_2026-11-14.json` | `recorded_vydra_probe` | same run, id `AAAAAAAAAAA` | refusal without metadata → `409 content_unavailable{other}` |
| `malformed/player_probe_without_playability_2026-11-14.json` | `derived_malformed` | the public capture with `playabilityStatus` deleted | `availability.state = "unknown"` and strict-mode refusal |
| **`synthetic/player_live_broadcast_storyboards.json`** | `synthetic` | hand-built for Phase 5B, shaped after the documented sections | live broadcast + storyboards + `likeCount` + formats + captions in one `OK` response |

## 2. The synthetic fixture in detail

`crates/vydra-providers/tests/fixtures/synthetic/player_live_broadcast_storyboards.json`
(sidecar: `…meta.json`)

* **Why it exists.** Phase 5B needed one document that carries `liveBroadcastDetails`, a storyboard spec,
  `likeCount`, `streamingData` and `captions` **together with an `OK` verdict**. No such capture exists:
  Phase 5A obtained no `OK` verdict (the runner's datacenter IP is bot-checked, §11 of the 5A report) and
  the 2025 recording had `storyboards` removed during reduction.
* **What is invented.** Every id, title, count, timestamp, URL and signature. URLs are
  `example.invalid`, the signature is the constant `SYNTHETICSIG`, and the dates are placeholders.
* **What it therefore proves.** Only that the parser handles this *shape*: `isLiveNow` → `live`,
  `startTimestamp` → `broadcast.start_at`, `publishDate` with a `-08:00` offset → the same instant in UTC
  with the offset kept, `lengthSeconds: "0"` → `Some(0)` (a value, not a gap), a two-level storyboard
  spec → geometry with no URL, a `signatureCipher` format → `signature_protected`.
* **What it does not prove.** That upstream emits any of it today, that the storyboard spec format is
  still `url|w#h#frames#cols#rows#interval#name#sigh`, that an `OK` response still looks like this, or that
  any URL resolves. The sidecar's `not_verified` list says the same thing in machine-readable form.

Nothing in the corpus is relabelled: the synthetic file is in `synthetic/`, its `class` is `synthetic`,
and its note states it is not a capture. The live captures were not edited; the synthetic fixture is an
addition beside them, not a substitute for them.

## 3. Raw captures

The Phase 5B tests read reduced fixtures only. The raw bytes they were reduced from stay immutable in
`docs/research/data/`:

| Directory | Content | Integrity |
| --- | --- | --- |
| `docs/research/data/player-probe-43549736295/` | four player responses (raw + gzip), a summary record, `manifest.json` | SHA-256 per file, gzip and raw, in `manifest.json` |
| `docs/research/data/player-probe-43549141233/` | the first run, including the pre-fix `age_restricted` misclassification | same |

`scripts/fixtures/import_player_capture.py <dir> --check` re-verifies the digests and re-derives the
fixtures, which is how the byte counts and the `213`/`214` disagreement in this document were confirmed.

## 4. Verification boundary

* A **passing fixture test proves the parser**, not that YouTube still answers this way. Phase 5A's
  captures are a snapshot from a bot-checked datacenter IP on 2026-11-14.
* A **successful HTTP 200** is not evidence of correct metadata: every Phase 5A refusal was also a 200.
  That is exactly why the document carries `availability` and `evidence` rather than a boolean.
* The storyboard and broadcast sections are **synthetic-only** and are labelled that way in the API
  contract (`docs/research/VIDEO_METADATA_CONTRACT.md` §6–7).
