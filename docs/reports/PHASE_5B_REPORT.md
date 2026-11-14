# Phase 5B report — Video metadata engine

**Result: IMPLEMENTED, TESTED AND DOCUMENTED — with one area still BLOCKED.** The player response is now
mapped into a provider-neutral document that reads every section the endpoint actually returns, states
where each value came from, and never fills a gap with a guess. The one thing this phase could not obtain
is a live `OK` verdict: the runner's datacenter IP is bot-checked (Phase 5A §11), so `available`, `live`,
`upcoming`, `broadcast` and `storyboards` remain fixture- or synthetic-verified. That is recorded in
§7 and in `docs/research/VIDEO_METADATA_CONTRACT.md` §7, not glossed over.

| Item | Value |
| --- | --- |
| Branch | `arena/06c8b7fc-vydra` (PR #1, left **open and unmerged**) |
| Starting commit | `2f0618b` (Phase 5A final, 22/22 green) |
| Final code commit | `0d1a97e` — 23/23 jobs green in run `43555445850`; the docs commit that carries this report follows it (§10) |
| CI | §9 |

---

## 1. What was asked, and what was done

| Requirement | Outcome |
| --- | --- |
| Establish the baseline; no change to `main`; do not merge PR #1 | **Done** (§2) |
| Provider-independent, stable video metadata model | **Done** (§3): no InnerTube shape reaches `vydra-domain`; `MetadataEvidence` makes provenance and confidence per document |
| Missing ≠ zero/empty; approximations never exact; original text preserved; UTC with source timezone kept; no fabrication | **Done** (§3, §4); the padding rules are pinned by tests, including `lengthSeconds: "0"` → `Some(0)` |
| Audit and improve the player mapping: `videoDetails`, `playabilityStatus`, `microformat`, `streamingData`, `captions`, `storyboards` | **Done** for all six; `storyboards` is newly implemented and **synthetic-only** (§6) |
| No bypass of bot checks, auth, restrictions; no signature/n-sig/PoToken; no playback-URL claims | **Upheld.** No new request was made in this phase; nothing from `signatureCipher`/`cipher`/`spec` URLs is stored |
| `GET /api/v1/videos/{id}`: id validation, stable schema, error mapping, provenance, OpenAPI | **Done** (§5); the schema went from `data: {type: object}` to a full document schema with an example |
| Cache: audit the abstraction, define behaviour per document class, bounded TTLs, no Redis | **Done** (§5, ADR-022); no new dependency, no backend change |
| Tests: domain, mapper, missing/malformed, timestamps, availability, API, cache, Phase 5A regressions | **Done** (§8) — 26 new tests |
| Fixture policy: real or explicitly labelled synthetic, provenance preserved, raw captures immutable | **Done** (§7); the policy is now enforced by a test, not only by prose |
| Optional live `OK` evidence | **BLOCKED** — see §7.3; documented, not faked |
| Preserve security/quality gates; fix regressions; do not weaken tests | **Done** (§9): every gate still runs; no test was deleted or relaxed |
| Docs: report + metadata contract + provenance; update PROGRESS/PROVIDER_ENGINE/OpenAPI/ADRs | **Done** (§10) |

## 2. Baseline

| Check | Result |
| --- | --- |
| `git status` | clean at `2f0618b`, branch `arena/06c8b7fc-vydra` |
| Last CI before this phase | run `43552753198` (push) and `43552757891` (PR): **22/22 green** |
| Local `cargo` | unavailable by design (no toolchain, no crates.io); CI is the compiler of record (ADR-017) |
| Documents read | `docs/PROGRESS.md`, `docs/reports/PHASE_5A_REPORT.md`, `PLAYER_RESPONSE_ANALYSIS.md`, `PLAYER_REQUEST_CONTRACT.md`, `API_CONTRACTS.md`, `PROVIDER_ENGINE.md`, `IMPLEMENTATION_DECISIONS.md` |
| Code read | `vydra-domain`, `innertube/{map,provider,client,request}.rs`, `vydra-engines`, `vydra-api`, `vydra-cache`, `vydra-config`, `vydra-provider-registry` |

Because the sandbox cannot compile, every claim below is tied to either a committed test or a CI run.

## 3. Domain model

New types in `vydra-domain` (all additive; every one is omitted from the wire form when absent):

| Type | Why it exists |
| --- | --- |
| `Publication` | `published_at` (UTC) **plus** `upstream_text` and `upstream_offset_seconds` — the live capture's `-07:00` offset is data, not formatting |
| `BroadcastInfo` | the scheduled/broadcast window (`live_now`, `start_at`, `end_at`) so `live`/`upcoming` are grounded in a stated window |
| `Storyboard` / `StoryboardLevel` | geometry and presence flags only: `url_template_present`, `sigh_present`, no URL field |
| `FieldSource` | `video_details`, `microformat`, `streaming_data`, `captions`, `storyboards`, `description`, `request` |
| `FieldFallback` | one field filled from the secondary section |
| `FieldConflict` | two sections disagreed: kept value, rejected value, both sources |
| `MetadataEvidence` | `sections[]`, `fallbacks[]`, `conflicts[]` — the "confidence" half of a document, stated as evidence rather than a probability |

`VideoDocument` gained `publication`, `broadcast`, `storyboards`, `unlisted`, `family_safe`, `evidence`;
`like_count` already existed but nothing ever filled it.

Rules the model enforces:

1. **Absent ≠ zero.** Every optional field is `Option`; `Some(0)` and `None` are distinct and tested.
2. **No provider shapes.** The domain has no `spec`, no `lengthSeconds`, no `renderer`; `map.rs` is the
   only module that knows InnerTube.
3. **Original text survives normalisation** (`Publication.upstream_text`).
4. **UTC on the wire, offset as data.** `published_at`/`start_at`/`end_at` are always UTC.
5. **Nothing invented.** `comment_count` is still always `None`: the player response does not carry a
   comment count, and the `next` endpoint (which does) is out of scope.

## 4. Player mapping

`crates/vydra-providers/src/innertube/map.rs::map_player` was rewritten around an explicit precedence
table (documented in `docs/research/VIDEO_METADATA_CONTRACT.md` §1–3):

* `videoDetails` primary, `microformat` fallback, both recorded.
* **Disagreements recorded, not averaged.** The live capture disagrees about duration — `213` in
  `videoDetails`, `214` in `microformat`. The document reports `213` and carries
  `evidence.conflicts[{field: duration_seconds, kept: 213, rejected: 214}]` plus a
  `conflict:duration_seconds:213!=214` parse message.
* **Newly mapped:** `likeCount` (exact `Count`, absent when unpublished), `publishDate` + `uploadDate`,
  `isUnlisted`, `isFamilySafe`, `category`, `liveBroadcastDetails`, `storyboards` geometry.
* **Availability refinement:** an `OK` verdict on a live/upcoming broadcast maps to
  `Availability::Live`/`Upcoming` instead of `Available`, so the cache policy can see it. Every Phase 5A
  verdict rule is unchanged (ADR-020 still governs).
* **Robustness:** every member of `playabilityStatus`, `videoDetails` and `microformat` is now read
  through a helper rather than deserialized into a concrete type, so a wrong-typed field costs **one
  field** instead of the whole document. Test:
  `a_wrong_typed_member_costs_one_field_not_the_document` (`lengthSeconds` as an object, `keywords` as a
  string, `isLiveNow` as `"true"`).
* **Present-but-unreadable is counted.** `lengthSeconds: "two minutes"` yields no duration and a
  `missing_field:videoDetails.lengthSeconds`; an *absent* `lengthSeconds` is upstream saying nothing and
  is not counted. This is the distinction between "a live broadcast has no duration" and "we lost it".
* **Storyboards** (§6) are parsed from the spec's level list; an unreadable spec yields no storyboards and
  a `storyboards_unparsed` warning.

## 5. API and cache

### 5.1 `GET /api/v1/videos/{id}`

* Video ids are validated before any provider call (`VideoId::parse`, `400 validation_failed`), unchanged.
* **Defect fixed:** the handler hard-coded `hl`/`gl` to `None`, so a localized title was unreachable even
  though the engine, the cache key and the provider all take a locale. `VideoQuery` now validates and
  normalises them the same way `/search` does; `/videos/{id}/metadata`, `/chapters` and `/captions` accept
  them too.
* Error mapping is unchanged: a refusal without metadata is `409 content_unavailable` with the reason, a
  refusal *with* metadata is a `200` whose `availability` says so.
* The OpenAPI document gained real schemas (`VideoDocument`, `VideoSummary`, `Count`, `Availability`,
  `Publication`, `Broadcast`, `Storyboard`, `MetadataEvidence`), the `hl`/`gl` parameters and a worked
  example taken from the live capture. `redocly lint` passes with the same five pre-existing warnings.
* Nothing session-scoped is exposed: no API key, no visitor id, no URL, no signature. Asserted by tests
  that search the serialized document for the fixture's placeholders.

### 5.2 Cache policy (ADR-022)

`DocumentCachePolicy` in `vydra-engines`, wired from `cache.metadata_ttl_secs` / `live_ttl_secs` /
`unavailable_ttl_secs`:

| Document | TTL |
| --- | --- |
| `available` | 300 s |
| `live`, `upcoming` | 30 s |
| `unavailable` | 60 s |
| `unknown` | never |
| upstream failure | never (an `Err` stores nothing) |

**Defect found:** the cache entry was stamped with a fingerprint of the provider **ids** only. The
InnerTube provider's `client_label()` carries the client version, which is part of the request context and
changes the body upstream returns; a client bump would therefore have kept serving the previous client's
documents. `ProviderRegistry::config_fingerprint()` now covers id **and** label, with a test.

No Redis, no new dependency: the in-memory cache is bounded and the contract is unchanged. Stale-while-
revalidate remains unimplemented (`stale_while_revalidate_secs` is configured but `MemoryCache` sets a zero
stale window); this report lists it under "not implemented" rather than claiming it.

## 6. Storyboards

Implemented from `storyboards.playerStoryboardSpecRenderer.spec`
(`url_template|w#h#frames#cols#rows#interval_ms#name#sigh|…`):

* the template — a signed image URL — is **never stored**; `url_template_present` and `sigh_present`
  record its existence;
* level geometry (width, height, frame count, columns, rows, interval, name) is kept;
* an unreadable or truncated spec produces no storyboards and one counted warning, never a guess;
* the two tests (`storyboard_spec_is_reduced_to_geometry_and_never_to_a_url`,
  `an_unreadable_storyboard_spec_yields_nothing_and_says_so`) assert that the serialized document contains
  neither the signature nor the template.

**Evidence status: synthetic-only.** `storyboards` was present in 0/4 Phase 5A captures and was removed
from the 2025 recording during reduction. The spec layout comes from the reference implementations
catalogued in `docs/research/SOURCE_CODE_MAP.md`; if upstream changes it, the parser fails closed with
`storyboards_unparsed`.

## 7. Evidence and verification boundaries

### 7.1 Live-verified (Vydra's own capture, run `43549736295`, 2026-11-14)

`likeCount` = `19503628`; `publishDate` = `uploadDate` = `2009-10-24T23:57:33-07:00` → `2009-10-25T06:57:33Z`
with offset `-25200` retained; `isUnlisted: false`; `isFamilySafe: true`; `category: Music`; and the
`213` vs `214` duration disagreement between the two sections.

### 7.2 Fixture-verified

`available` (`recorded/player_web_2025-11-10.json`), formats, caption tracks, chapters, and the
`unknown` path (`malformed/player_probe_without_playability_2026-11-14.json`).

### 7.3 Blocked

**A live `OK` verdict.** Phase 5A could not obtain one: the CI runner's datacenter IP is bot-checked, and
obtaining one would need a legitimate non-datacenter egress (a maintainer's machine or a self-hosted
runner). This phase made no new upstream request at all. Consequences, stated plainly:

* `Availability::Available`, `Live` and `Upcoming` are exercised only by fixtures, one of which is
  synthetic;
* `broadcast` and `storyboards` are **synthetic-only**;
* format and caption metadata remain fixture-verified.

### 7.4 Synthetic-only

`synthetic/player_live_broadcast_storyboards.json` — the single `OK` document in the corpus. Its sidecar
says it is not a capture, lists the sections it exercises and lists what it does not prove; a test
(`a_synthetic_fixture_says_it_is_not_a_capture`) enforces that every synthetic fixture does.

## 8. Tests added

| Area | Tests | Where |
| --- | --- | --- |
| Domain value objects | 2 (`publication_keeps_the_upstream_text_and_offset`, `evidence_omits_empty_arrays_and_records_conflicts`) | `vydra-domain/src/common.rs` |
| Mapper (microformat, conflicts, broadcast, storyboards, malformed) | 11 | `innertube/map.rs` |
| Live + synthetic integration | 2 | `tests/player_parser.rs` |
| Fixture hygiene | 3 | `tests/fixture_provenance.rs` |
| Cache policy + keys + fingerprint | 6 | `vydra-engines/src/lib.rs` |
| Configuration fingerprint | 1 | `vydra-provider-registry/src/lib.rs` |
| API locale handling | 1 | `vydra-api/src/lib.rs` |

Phase 5A regressions are not re-tested by new code, they are **kept**: `tests/player_parser.rs` still
pins all six 5A defects (page/age substring, content fact as parse error, missing verdict defaulting to
`available`, ignored `messages`/`errorScreen`, ignored `videoDetails.thumbnail`, missing `videoId`).

## 9. CI

| Run | Commit | Purpose | Result |
| --- | --- | --- | --- |
| `43553918750` | `cc531c8` | first Phase 5B commit | red: 2 compile errors (`live_state_of` name collision with the search helper, partial move in a test), 2 unused imports in `vydra-domain` |
| `43554393169` | `476d588` | rename + import fixes | red: one partial move left in an integration test (borrowing `&document` after moving a field out of it) |
| `43554917627` | `5e67ac8` | borrow fixes | red: `fmt` on the unformatted commit (the bot's `style:` commit followed), and `fixture_provenance` tripped on `malformed/search_truncated.json`, which is invalid JSON **by design** |
| **`43555445850`** | **`0d1a97e`** | contract docs + fixture-test fix | **23/23 green** (22 permanent checks + the temporary `fmt-apply`) |

Each red run exposed a real defect, not a CI quirk: a duplicate helper name that would have shadowed the
search mapper's `live_state_of`, a partial move, and a fixture-hygiene assertion that had not accounted for
the one class whose whole purpose is invalid input.

`fmt` was applied by the temporary `fmt-apply` job (gated on a commit-message token), which pushes a
`style: apply rustfmt` commit. It is removed in the final commit of this phase, restoring the 22-check
baseline.

## 10. Files changed

Source: `crates/vydra-domain/src/{common,video,lib}.rs`,
`crates/vydra-providers/src/innertube/map.rs`, `crates/vydra-engines/src/lib.rs`,
`crates/vydra-provider-registry/src/lib.rs`, `crates/vydra-config/src/lib.rs`,
`crates/vydra-api/src/lib.rs`, `apps/api-server/src/main.rs`.
Tests: `crates/vydra-providers/tests/{player_parser.rs,fixture_provenance.rs}`,
`crates/vydra-providers/tests/fixtures/synthetic/player_live_broadcast_storyboards.{json,meta.json}`,
`…/synthetic/search_mixed.meta.json` (`not_verified` added).
Docs/API: `docs/api/openapi.yaml`, `docs/research/{VIDEO_METADATA_CONTRACT,VIDEO_METADATA_FIXTURE_PROVENANCE}.md`,
`docs/reports/PHASE_5B_REPORT.md`, `docs/PROGRESS.md`, `docs/architecture/{PROVIDER_ENGINE,API_CONTRACTS}.md`,
`docs/research/{IMPLEMENTATION_DECISIONS,PLAYER_RESPONSE_ANALYSIS,FIXTURE_PROVENANCE}.md`,
`crates/vydra-providers/tests/fixtures/README.md`.
CI: `.github/workflows/ci.yml` (temporary `fmt-apply`, removed before the final run).

## 11. Remaining limitations and risks

1. **No live `OK` capture** (§7.3). This is the single largest gap and it caps what can be claimed about
   `available`/`live`/`upcoming`, formats, captions and storyboards.
2. **Storyboard and broadcast shapes are synthetic-only** (§6, §7.4). They fail closed, but a wrong
   assumption would only be visible as a `storyboards_unparsed` warning in production.
3. **Wording-based availability classification can lag upstream.** A new phrasing maps to `other` and
   surfaces the raw status; unchanged from 5A.
4. **`comment_count` is not mapped.** The player response does not carry it; doing so needs the `next`
   endpoint (Phase 5C at the earliest).
5. **One locale axis is tested only structurally.** `hl`/`gl` now reach the cache key and the provider, but
   no live capture exists for a non-`en`/`US` combination (Phase 5A §12 recommends exactly that matrix).
6. **Stale-while-revalidate is configured but not implemented** (`MemoryCache` stale window is zero).
7. **Coherence between `summary.published_at` and `publication`** is by construction, not by a database
   constraint; a future writer must keep them in step (only `map_player` writes them today).

## 12. Next recommendation

1. **One `OK` capture from a non-datacenter egress** — the highest-value evidence left, and the only way to
   move `available`/`live`/formats/captions/storyboards from fixture to live.
2. **An `hl`/`gl` matrix over the existing probe inputs**, which would test the locale axis this phase
   wired up end to end.
3. Then the `next` endpoint, which unlocks related videos and the comment count.
