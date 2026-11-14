# Video metadata contract

What Vydra promises about a video document: which upstream section every field comes from, what happens
when two sections disagree, what "absent" means, and how each claim is verified.

Phase 5B. Companion documents: `docs/research/PLAYER_RESPONSE_ANALYSIS.md` (what the player endpoint
returned to Vydra), `docs/research/VIDEO_METADATA_FIXTURE_PROVENANCE.md` (which bytes prove what),
`docs/reports/PHASE_5B_REPORT.md` (what is implemented, tested and still pending).

Verification vocabulary, used with the same meaning everywhere in this repository:

| Term | Meaning |
| --- | --- |
| **live-verified** | the shape was observed in a response Vydra itself received through its production transport, on a stated date and run |
| **fixture-verified** | the shape is exercised by a committed fixture: either Vydra's own capture (reduced) or a third-party recording, named in the provenance document |
| **synthetic-only** | only a hand-built fixture covers it; no capture of Vydra's shows this shape |
| **blocked** | it cannot be obtained in this environment, and the reason is recorded |

---

## 1. The document

`GET /api/v1/videos/{id}` returns `data` as a `VideoDocument`:

| Field | Type | Source | Absent means |
| --- | --- | --- | --- |
| `summary.id` | video id | the request (cross-checked against `videoDetails.videoId`) | never absent; a mismatch is a parse error |
| `summary.title` | string | `videoDetails.title`, else `microformat…title` | never absent; an empty title is a counted parse loss |
| `summary.channel_id` | channel id | `videoDetails.channelId`, else `microformat…externalChannelId` | upstream did not state a channel |
| `summary.channel_title` | string | `videoDetails.author`, else `microformat…ownerChannelName` | as above |
| `summary.duration_seconds` | u32 | `videoDetails.lengthSeconds`, else `microformat…lengthSeconds` | not stated (a live broadcast often has none); **0 is a value** |
| `summary.view_count` | `Count` | `videoDetails.viewCount`, else `microformat…viewCount` | not stated |
| `summary.published_at` | RFC 3339 UTC | `microformat…publishDate` (normalised from `publication`) | upstream stated no date |
| `summary.thumbnails` | `Thumbnail[]` | `microformat…thumbnail`, else `videoDetails.thumbnail` | no image was listed |
| `summary.live_state` | enum | `videoDetails.isLiveNow/isUpcoming/isLiveContent`, else `microformat…liveBroadcastDetails` | upstream did not state a broadcast state |
| `availability` | enum | `playabilityStatus.status` (+ live state) | never absent; `unknown` when there is no verdict |
| `description` | string | `videoDetails.shortDescription`, else `microformat…description` | no description |
| `category` | string | `microformat…category` | not stated |
| `keywords` | string[] | `videoDetails.keywords` | none listed |
| `like_count` | `Count` | `microformat…likeCount` (exact) | **not published** (YouTube hides it on some videos) — never 0 |
| `comment_count` | `Count` | not mapped in this phase (the player response does not carry it) | always absent |
| `chapters` | `Chapter[]` | derived from the description | fewer than two timestamps is not a chapter list |
| `caption_tracks` | `CaptionTrack[]` | `captions.playerCaptionsTracklistRenderer` | no caption metadata; the URL is never echoed |
| `formats` | `MediaFormatInfo[]` | `streamingData` | no format metadata; no URL field exists at all (ADR-006) |
| `storyboards` | `Storyboard` | `storyboards.playerStoryboardSpecRenderer.spec` | not present, or the spec could not be read (counted warning `storyboards_unparsed`) |
| `publication` | `Publication` | `microformat…publishDate`/`uploadDate` | no publication text |
| `broadcast` | `BroadcastInfo` | `microformat…liveBroadcastDetails` | upstream stated no window |
| `unlisted` | bool | `microformat…isUnlisted` | not stated |
| `family_safe` | bool | `microformat…isFamilySafe` | not stated |
| `evidence` | `MetadataEvidence` | computed | omitted when empty |
| `provenance` | `Provenance` | the provider | never absent |

Two rules hold for every row:

* **Missing is not zero and not empty.** A `null`/absent field means upstream did not state it; `0`
  means upstream stated zero. `duration_seconds: 0` and `duration_seconds: null` are different facts and
  the map keeps them apart.
* **Nothing is inferred.** No field is computed from a guess, a default, or a request parameter. The
  requested id is the one exception, and it is cross-checked against the response.

## 2. Timestamps

ADR-016 requires RFC 3339 **UTC** on the wire. The live capture publishes
`"publishDate": "2009-10-24T23:57:33-07:00"`, so normalisation is real work, and it is lossy unless the
original is kept:

| Field | Value for the live capture |
| --- | --- |
| `publication.published_at` | `2009-10-25T06:57:33Z` |
| `publication.upstream_text` | `2009-10-24T23:57:33-07:00` |
| `publication.upstream_offset_seconds` | `-25200` |
| `publication.uploaded_at` | `2009-10-25T06:57:33Z` (from `uploadDate`, same instant here) |
| `summary.published_at` | `2009-10-25T06:57:33Z` — same instant, kept for clients that only need a timestamp |

* An unparsable `publishDate` is a **counted** loss (`missing_field:microformat.publishDate`) and yields
  no date. It is never replaced by "now", by `uploadDate`, or by a neighbouring field.
* `broadcast.start_at` / `end_at` are normalised the same way.
* `live_state` is derived from `liveBroadcastDetails` without inventing a clock: `isLiveNow: true` is
  `live`; `isLiveNow: false` with an `endTimestamp` is `ended`; `isLiveNow: false` with a future
  `startTimestamp` is `upcoming`; a past one is `not_live`.

## 3. Precedence and conflicts

`videoDetails` is primary; `microformat` fills gaps. When both carry a field:

| Outcome | What the document says | Where it is visible |
| --- | --- | --- |
| both agree | the value | nothing (a silent agreement needs no note) |
| disagree | the **primary** value | `evidence.conflicts[]` with both values and both sources |
| primary absent | the microformat value | `evidence.fallbacks[]` |

**This is not hypothetical.** In the live capture the two sections disagree about duration:

```
videoDetails.lengthSeconds      = "213"
microformat…lengthSeconds       = "214"
```

Vydra reports `213` (the value that governs playback) and records
`{"field":"duration_seconds","kept":"video_details","kept_value":"213","rejected":"microformat","rejected_value":"214"}`.
A document that silently picked one number would be unfalsifiable; this one can be audited.

The same disagreement check runs on `view_count` (agreement in the capture: `1836979877` in both) and
would run on any field added to `resolve_number`.

## 4. Availability

Unchanged from Phase 5A (ADR-020) except for the live/upcoming refinement:

| Verdict | `videoDetails` | Result |
| --- | --- | --- |
| `OK` | present, `isLiveNow: true` | `{"state":"live"}` |
| `OK` | present, `isUpcoming: true` | `{"state":"upcoming"}` |
| `OK` | present | `{"state":"available"}` |
| not `OK`, reason classified | present | `{"state":"unavailable","reason":…}` + `playability:<STATUS>` |
| not `OK` | absent | `409 content_unavailable` with the reason |
| absent | present | `{"state":"unknown"}` + counted `missing_field`; never cached |

`live`/`upcoming` are not cosmetic: they are what makes the cache policy (§5) treat the document as
fast-moving.

## 5. Cache policy

The TTL follows the **fact**, not the endpoint (ADR-022):

| Document | TTL | Reason |
| --- | --- | --- |
| `available` | `cache.metadata_ttl_secs` (300 s) | a published upload's metadata is stable for minutes |
| `live` / `upcoming` | `cache.live_ttl_secs` (30 s) | viewer counts, end time and state move while it airs |
| `unavailable` | `cache.unavailable_ttl_secs` (60 s) | content facts are reversible: private → public |
| `unknown` | **never** | the next request may get a verdict |
| any upstream failure | **never** | errors are `Err`, nothing is stored, so an outage cannot be remembered as unavailability |
| partial metadata (warnings/conflicts) | as its availability state | a recorded conflict is a statement about the parse, not a reason to distrust the data |

Cache key: `video:{id}:{hl}:{gl}`. The client profile is not in the key because it cannot change at
runtime; it is part of the configuration fingerprint that stamps every entry, so a client bump
invalidates the whole class instead of serving one client's body under another's configuration.

## 6. Storyboards

`storyboards.playerStoryboardSpecRenderer.spec` is
`url_template|width#height#frames#cols#rows#interval_ms#name#sigh|…`. Vydra keeps the geometry and the
presence flags, and drops the template: it is a signed image URL, which is the same class of material as
a playback URL (ADR-006), so it is never stored, logged or returned.

* `url_template_present` records that a template existed.
* `sigh_present` records that the level was signed.
* A spec this build cannot read yields **no** storyboards plus a counted warning
  `storyboards_unparsed` — never a partial guess.

**Evidence status: synthetic-only.** No Vydra capture contains `storyboards` (0/4 in Phase 5A) and the
2025 recording had the section removed during reduction. The format above comes from the reference
implementations catalogued in `docs/research/SOURCE_CODE_MAP.md`; nothing here proves YouTube still emits
it.

## 7. Verification matrix

| Field group | Status | Basis |
| --- | --- | --- |
| `summary.title`, `channel_*`, `duration_seconds`, `view_count`, `keywords`, `description` | **live-verified** | Phase 5A capture, run `43549736295` |
| `like_count`, `publication`, `unlisted`, `family_safe`, `category` | **live-verified** | same capture (`likeCount: 19503628`, `publishDate` with `-07:00`) |
| duration conflict `213` vs `214` | **live-verified** | same capture, both sections present |
| `availability` verdicts | **live-verified** (refusals) | `UNPLAYABLE`, `LOGIN_REQUIRED`, `ERROR` captures |
| `availability` = `available`/`live`/`upcoming` | **fixture-verified** | `recorded/player_web_2025-11-10.json` (available); `synthetic/player_live_broadcast_storyboards.json` (live) — **no live `OK` capture exists (blocked)** |
| `broadcast` | **synthetic-only** | `liveBroadcastDetails` never observed live |
| `storyboards` | **synthetic-only** | never observed live |
| `formats` | **fixture-verified** | 2025 recording; live answers were refused playback |
| `caption_tracks` | **fixture-verified** | 2025 recording |
| `chapters` | **fixture-verified** | derived from the live description's timestamp lines |
| `comment_count` | **not implemented** | the player response does not carry it; a comment count needs the `next` endpoint |

## 8. Reproducing the checks

```bash
# the live microformat facts, straight from the archived bytes
gunzip -c docs/research/data/player-probe-43549736295/player_public.json.gz \
  | jq '.microformat.playerMicroformatRenderer | {likeCount, publishDate, uploadDate, isUnlisted, isFamilySafe, lengthSeconds}'

# the disagreement the mapper records
gunzip -c docs/research/data/player-probe-43549736295/player_public.json.gz \
  | jq '{details: .videoDetails.lengthSeconds, microformat: .microformat.playerMicroformatRenderer.lengthSeconds}'

# the fixture tests
cargo test -p vydra-providers --test player_parser
cargo test -p vydra-providers --test fixture_provenance
```
