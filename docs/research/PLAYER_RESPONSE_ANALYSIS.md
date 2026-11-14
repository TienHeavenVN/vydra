# Player response analysis

What `POST /youtubei/v1/player` actually returned to Vydra, what the parser does with it, and what
remains unverified.

Phase: 5A. Captures: CI runs `43549141233` (2026-11-14T02:59:51Z) and `43549736295`
(2026-11-14T03:05:36Z), both archived under `docs/research/data/player-probe-*/` with per-file
SHA-256. Fixtures derived from the second run are listed in
`docs/research/PLAYER_FIXTURE_PROVENANCE.md`. Request shape: `docs/research/PLAYER_REQUEST_CONTRACT.md`.

---

## 1. Requests attempted

Four player requests per run, against three fixed ids — no enumeration, no retry, no playback request:

| Request | Video id | Client | Flags | Purpose |
| --- | --- | --- | --- | --- |
| `public` | `dQw4w9WgXcQ` | WEB | `contentCheckOk=true`, `racyCheckOk=true` | the "normal public video" case |
| `public_no_checks` | `dQw4w9WgXcQ` | WEB | `false`, `false` | does the verdict depend on the flags? |
| `captions` | `9Umm2MAKsMc` | WEB | `true`, `true` | a video known to carry caption tracks |
| `missing` | `AAAAAAAAAAA` | WEB | `true`, `true` | a syntactically valid id that should not resolve |

The probe then re-requests three of them **through the provider** (`InnertubeProvider::fetch`,
`ProviderRequest::Video`) so that the public-API projection is recorded next to the raw bytes — seven
player requests per run in total, still against three fixed ids and still with no retry.

`hl=en`, `gl=US`, client version from the `sw.js_data` bootstrap (`2.20260805.01.00`), `WEB` name id
`1`. No age-gated, region-gated, private, members-only, live or DRM id was requested: Vydra has no way
to obtain one without either guessing ids or exercising an access path it does not have.

## 2. Status distribution

Every request returned **HTTP 200 with `application/json; charset=UTF-8`**. The interesting part is the
playability verdict inside the body, not the HTTP status:

| Verdict | Requests | `videoDetails` present | Bytes (uncompressed, run 2) |
| --- | --- | --- | --- |
| `UNPLAYABLE` ("The page needs to be reloaded.") | 2 (`public`, `public_no_checks`) | **yes** | 14 534 / 14 506 |
| `LOGIN_REQUIRED` ("Sign in to confirm you're not a bot") | 1 (`captions`) | no | 7 539 |
| `ERROR` ("This video is unavailable") | 1 (`missing`) | no | 2 235 |

Both runs produced the same verdicts for the same ids, 6 minutes apart.

**Reading of this distribution.** The runner's datacenter IP is bot-checked by the player endpoint: the
public video is served *metadata but no playback*, and a second public video is refused outright. That
is an upstream access decision, not a Vydra defect — the search endpoint answered normally in Phase 4.3
from the same runner — and it is recorded rather than worked around. Nothing here was retried, and no
"verify it's you" flow was attempted.

## 3. Response shape actually observed

Union of the top-level keys across the four captures:

```
playabilityStatus, videoDetails, microformat, responseContext, frameworkUpdates, trackingParams,
adBreakHeartbeatParams, messages
```

| Section | Present in | Notes |
| --- | --- | --- |
| `playabilityStatus` | 4/4 | keys observed: `status`, `reason`, `errorScreen`, `skip`, `contextParams`. `messages` (the playability one) was **absent** in every capture |
| `videoDetails` | 2/4 (both `UNPLAYABLE`) | keys: `allowRatings`, `author`, `channelId`, `isCrawlable`, `isLiveContent`, `isOwnerViewing`, `isPrivate`, `isTvfilmVideo`, `isUnpluggedCorpus`, `keywords`, `lengthSeconds`, `shortDescription`, `thumbnail`, `title`, `videoId`, `viewCount` |
| `microformat` | 2/4 | only `playerMicroformatRenderer`, with `availableCountries`, `canonicalUrl`, `category`, `description`, `embed`, `externalChannelId`, `externalVideoId`, `hasYpcMetadata`, `isFamilySafe`, `isShortsEligible`, `isUnlisted`, `lengthSeconds`, `likeCount`, `ownerChannelName`, `ownerProfileUrl`, `publishDate`, `thumbnail`, `title`, `uploadDate`, `viewCount` |
| `streamingData` | **0/4** | not returned for a refused verdict. Format metadata therefore stays fixture-verified (§7) |
| `captions` | **0/4** | same: the caption track list lives in a playable response |
| `storyboards` | **0/4** | parsed since Phase 5B, but never observed — synthetic-only |

Two distinct `errorScreen` renderers appeared, which matters because the parser must not assume one:

* `playerErrorMessageRenderer` — `public`: an icon, a `proceedButton` whose command is
  `signalAction: RELOAD_PAGE`, and the reason text.
* `playerInterstitialRenderer` — `captions`, `missing`: a title/description pair rendered as a modal
  ("Video unavailable", "Sign in").

Values that the mapper used, from the `public` capture (2026-11-14):

| Field | Upstream | Mapped to |
| --- | --- | --- |
| `videoDetails.title` | `Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)` | `summary.title` |
| `videoDetails.lengthSeconds` | `"213"` | `summary.duration_seconds = 213` |
| `videoDetails.viewCount` | `"1836979877"` | `summary.view_count` = exact 1 836 979 877 |
| `videoDetails.channelId` | `UCuAXFkgsw1L7xaCfnd5JJOw` | `summary.channel_id` |
| `videoDetails.author` | `Rick Astley` | `summary.channel_title` |
| `videoDetails.keywords` | 27 entries | `keywords` |
| `videoDetails.shortDescription` | 2 375 chars | `description` (+ chapter extraction input) |
| `videoDetails.thumbnail.thumbnails` | 5 sizes | thumbnails **fallback** (microformat wins when present) |
| `microformat…publishDate` | `2009-10-24T23:57:33-07:00` | `published_at = 2009-10-25T06:57:33Z` (offset applied) |
| `microformat…category` | `Music` | `category` |
| `microformat…thumbnail` | one 1280×720 entry | `summary.thumbnails` |

Phase 5B added the fields this capture carried and the 5A mapper did not read:

| Field | Upstream | Mapped to |
| --- | --- | --- |
| `microformat…likeCount` | `"19503628"` | `like_count` = exact 19 503 628 |
| `microformat…publishDate` | `2009-10-24T23:57:33-07:00` | `publication.published_at` (UTC) **and** `upstream_text`/`upstream_offset_seconds` (`-25200`) |
| `microformat…uploadDate` | same instant | `publication.uploaded_at`/`uploaded_text` |
| `microformat…isUnlisted` | `false` | `unlisted` |
| `microformat…isFamilySafe` | `true` | `family_safe` |
| `microformat…lengthSeconds` | `"214"` | **rejected** — `evidence.conflicts[{kept: 213, rejected: 214}]` |
| `microformat…externalChannelId`, `ownerChannelName`, `title`, `description` | present | fallbacks when `videoDetails` lacks the field |
| `videoDetails.isLiveContent` | `false` | `live_state = not_live` |
| `liveBroadcastDetails`, `storyboards` | **absent** | absent in the document too; synthetic-only elsewhere |

## 4. Verdict → taxonomy mapping

The rule (ADR-020): **the verdict is a value, not an error**; whether there is a document is a separate
question. The mapping is applied in `innertube/map.rs::map_player` and pinned by
`tests/player_parser.rs`.

| Upstream verdict | `videoDetails` | Vydra result | HTTP | Evidence |
| --- | --- | --- | --- | --- |
| `OK` | present | document, `availability.state = "available"` | 200 | `recorded/player_web_2025-11-10.json` (third-party recording, 2025-11-10) |
| `UNPLAYABLE` / `ERROR` / `LOGIN_REQUIRED` etc., reason names a restriction | present | document, `availability = {state:"unavailable", reason:<mapped>}` + counted message `playability:<STATUS>` | 200 | live `UNPLAYABLE` capture, reason unclassifiable → `other` |
| same, no `videoDetails` | absent | `ProviderError::Unavailable { reason, message }` → `409 content_unavailable` | 409 | live `LOGIN_REQUIRED` and `ERROR` captures |
| `playabilityStatus` absent, or `status` absent | present | document, `availability.state = "unknown"` + `missing_field:playabilityStatus.status` (strict mode → `provider_parse`) | 200 | transformed capture `malformed/player_probe_without_playability_2026-11-14.json` |
| unknown status | either | never `available`: document with `reason: other`, or 409 when there is no document | 200/409 | unit tests `an_unrecognised_playability_status_is_never_success` |

Reason selection uses **whole words** from `reason`, `playabilityStatus.messages[*].text` and
`errorScreen` text:

| Wording | Reason | Evidence |
| --- | --- | --- |
| `private` | `private` | wording only (no live sample) |
| `members`, `membership` | `members_only` | wording only |
| `country`, `region` | `geo_restricted` | wording only |
| `age` | `age_restricted` | wording only — the age gate puts this in `messages`, not `reason` |
| `copyright` | `copyright_blocked` | wording only |
| `drm`, "protected content" | `drm_protected` | wording only |
| `deleted`, `removed`, "no longer available" | `deleted` | wording only |
| `premiere`, "live stream" | `not_broadcasting` | wording only |
| `LOGIN_REQUIRED` and nothing more specific | `login_required` | **live** (bot check) |
| `ERROR` + "This video is unavailable" | `other` | **live** — upstream does not say whether the video was deleted or never existed, so claiming `deleted` would be invention |

## 5. Defects this capture exposed

1. **Substring matching invented an age restriction.** The first analysis pass mapped
   `UNPLAYABLE` + "The page needs to be reloaded." to `age_restricted`, because `"page"` contains
   `"age"`. The live provider mapping in run `43549141233` shows the wrong value
   (`"availability": {"reason":"age_restricted"}`), which is exactly the class of bug a fixture written
   by hand never catches. Fixed with whole-word/phrase matching (`map.rs::mentions`) and a regression
   test using the real string.
2. **A content fact was reported as a parse failure.** With no `videoDetails`, the old order
   (require `videoDetails` first) returned `provider_parse` for the bot check and for the non-existent
   id — blaming Vydra's parser for an upstream decision. The verdict is now classified first, and a
   restricted answer without metadata becomes `ProviderError::Unavailable` (409).
3. **A missing verdict manufactured availability.** A payload with no `playabilityStatus` used to map to
   `availability.state = "available"` and would have been cached and served as playable. It is now
   `Availability::Unknown`, which `is_available()` rejects and which is never cached.
4. **`playabilityStatus.messages` / `errorScreen` were ignored**, so an age gate that explains itself
   only in `messages` could not be classified.
5. **`videoDetails.thumbnail` was ignored** when `microformat` was absent: a real document would map to
   zero thumbnails because one optional section was missing. Now used as a fallback (5 sizes in the live
   capture).
6. **A missing `videoDetails.videoId` was accepted**, with the id silently taken from the request. It is
   now a parse failure: identity must come from the payload, or there is no document.

## 6. What the capture does *not* prove

* That upstream answers the same way for a residential IP, another region, another client profile or
  after another week. The verdicts are a snapshot from a bot-checked datacenter IP.
* That `streamingData`, `captions` or `storyboards` still look like the 2025 recording. For this phase
  they remain **fixture-verified only** (`unverified_live`).
* That any playback URL works, or that Vydra could obtain one. Obtaining one is out of scope.

## 7. Per-section verification status

| Section | Status | Basis |
| --- | --- | --- |
| `playabilityStatus.status` / `.reason` | **VERIFIED_LIVE** | 4/4 captures, three distinct statuses |
| `playabilityStatus.errorScreen` (both renderers) | **VERIFIED_LIVE** | `playerErrorMessageRenderer`, `playerInterstitialRenderer` |
| `playabilityStatus.messages` | **VERIFIED_FIXTURE** | absent live; classification tested from a synthetic string |
| `videoDetails` metadata set | **VERIFIED_LIVE** | title, duration, views, channel, author, keywords, description, thumbnails |
| `microformat.playerMicroformatRenderer` | **VERIFIED_LIVE** | publishDate, category, owner, thumbnails |
| `streamingData` | **UNVERIFIED_LIVE / VERIFIED_FIXTURE** | third-party 2025 recording only |
| `captions.playerCaptionsTracklistRenderer` | **UNVERIFIED_LIVE / VERIFIED_FIXTURE** | same |
| `storyboards` | **SYNTHETIC_ONLY** (parser added in Phase 5B) | no capture carries it; the geometry parser is exercised only by `synthetic/player_live_broadcast_storyboards.json` |

## 8. Reproducing this analysis

```bash
gunzip -c docs/research/data/player-probe-43549736295/player_public.json.gz | jq '.playabilityStatus'
gunzip -c docs/research/data/player-probe-43549736295/summary.json.gz | jq '.records[] | {label, status, verdict}'
python3 scripts/fixtures/import_player_capture.py docs/research/data/player-probe-43549736295 --check
```
