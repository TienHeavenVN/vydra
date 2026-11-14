# Playlist contract

What Vydra claims about a playlist, what it claims about each entry, and what it refuses to claim.

Normative code: `crates/vydra-domain/src/playlist.rs`, `crates/vydra-providers/src/innertube/browse_map.rs`
(`map_playlist`, `map_playlist_videos`), `crates/vydra-providers/src/innertube/request.rs`. Provenance:
`docs/research/CHANNEL_PLAYLIST_FIXTURE_PROVENANCE.md`. Verification boundaries: `docs/reports/PHASE_6_REPORT.md` §7.

## 1. The wire request

A playlist is a `browse` call with `browseId: "VL" + <playlist id>` and the same `context` object every other
request uses (`hl`, `gl`, client profile, visitor data).

`VL…` is the **only** string Vydra concatenates onto an id, and it is documented upstream (`vl` prefix, "video list").
Nothing else is encoded locally:

* no hand-built continuation blobs. `NewPipeExtractor` sends `params: "wgYCCAA%3D"` to force the first page of entries
  and `"CADCBgIIAA%3D%3D"` to force the next; those are proto-encoded blobs whose meaning is not documented, and their
  continued correctness is not verifiable from here. Vydra reads the entries the page returns and takes its
  continuations from `continuationItemRenderer` (§5), exactly as the request layer does for search and player;
* no playlist `params` for sorting or filtering. A sort parameter would be an unverified encoding, and the API does
  not expose sorting.

`GET /api/v1/playlists/{id}` and `GET /api/v1/playlists/{id}/videos` therefore issue **one** request for page 1, and
one further request per continuation page, and nothing else.

## 2. The document

| Field | Source | Absent means |
| --- | --- | --- |
| `id` | the requested id, echoed | the page carries no trustworthy id of its own, so a redirect to another list cannot be detected; the document always names the list that was asked for |
| `title` | `playlistHeaderRenderer.title`, legacy `sidebar` title, `playlistMetadataRenderer.title`; first hit wins and later hits are recorded as fallbacks | no title on the page |
| `description` | `playlistHeaderRenderer.descriptionText` | no description |
| `channel_id`, `channel_title` | `playlistHeaderRenderer.ownerText` / `videoOwnerRenderer` byline | the page states no owner (e.g. a system list) |
| `thumbnails` | `playlistHeaderBanner` / the first entry's thumbnail as a fallback | no thumbnail |
| `item_count` | `numVideosText`, else the sidebar's first stat | the page did not state a count |
| `view_count` | sidebar stats, when stated | absent — a playlist page often states none |
| `last_updated_text` | sidebar's last stat, kept verbatim | absent |
| `availability` | a returned document is always `available` | an alert that maps to "private"/"missing" is an `Err`, not a document (see §6) |

`item_count` counts **entries the owner has**, which is not always the number of entries Vydra can return: a private or
deleted entry still occupies a position. The document therefore never reconciles `item_count` against
`items.len()`, and a client must not compute the difference as "missing videos" without also reading
`repeated_entries`.

## 3. Entries and positions

`PlaylistItem` is the entry model:

| Field | Rule |
| --- | --- |
| `position` | the row's own `index` (1-based, as rendered). `None` when the row states none — a `lockupViewModel` has no `index`. It is **never** computed from the offset in the page, because a continuation page starts at an arbitrary offset and a computed position would contradict the number the owner sees |
| `video_id` | validated `VideoId`, or `None`. A deleted/private entry is rendered as a title with no id, and a lookalike string is not an id |
| `title` | verbatim, including `[Deleted video]` / `[Private video]` |
| `channel_id`, `channel_title` | the entry's byline, when the row renders one |
| `duration_seconds` | `lengthSeconds` (exact) or a `9:59` badge; never a label |
| `thumbnails` | the row's thumbnail sources, ordered smallest → largest |
| `availability` | `isPlayable: false` → `Unavailable{reason: private/deleted/…}`; `isPlayable` **absent** → `Unknown`; never `Available` by default |
| `source` | which renderer produced the entry (`playlist_video`, `lockup`, `other`) |

**Positions are preserved, not renumbered.** An unavailable entry stays in the list at its position with
`video_id: None` and a non-available `availability`. Skipping it would shift every later position and silently
reorder the playlist, which is the failure mode this contract exists to prevent.

## 4. Duplicates

A playlist may legitimately contain the same video twice (an owner can add it twice). Vydra counts such repeats in
`repeated_entries` and keeps them: the count lets a client distinguish "the owner added it twice" from "the mapper
emitted it twice". Nothing is dropped, and positions are untouched.

## 5. Continuations

`next_cursor` is minted only from a genuine page continuation:

* `continuationItemRenderer.continuationEndpoint.continuationCommand.token` (modern rows) or a
  `nextContinuationData` / `continuationCommand` inside the entry list (older envelope);
* tokens that appear on filter chips, sort menus or unrelated buttons are refused (`parse::find_page_cursor` checks
  the enclosing structure, not the token's shape);
* a token identical to the one that produced the current page yields `next_cursor: None` plus a warning, so a client
  cannot be walked into an unbounded loop;
* a continuation request whose response carries no page cursor is reported (`continuation_without_page_cursor`) and
  ends the walk instead of repeating the request;
* a page whose entry list is empty while the header stated no item count is reported (`playlist_entries_missing`),
  and a bound that truncated the page is reported (`page_truncated`), so an empty page is never presented as "the
  playlist has nothing more".

## 6. Availability, alerts and refusals

The page's alerts are translated only for the verdicts the ADRs list (`ADR-021`): a `WARNING`/`ERROR` alert whose text
matches "private", "does not exist" or "unavailable" becomes `Unavailable{reason}` with the alert text preserved in
`evidence`; any other alert text is recorded verbatim and changes nothing. Vydra does not guess a verdict from an
unknown localized sentence.

A playlist page that renders **no** header and **no** entry list is a parse failure, not an empty playlist: an empty
playlist renders `0 videos` and an empty list container. Nothing that a bot check, a consent wall or a sign-in gate
returns is treated as a document.

## 7. Cache policy

| Resource | TTL | Why |
| --- | --- | --- |
| playlist document | `listing_ttl_secs` (120 s) | titles and counts change rarely; entry pages move |
| entries, page 1 | `listing_ttl_secs` (120 s) | an owner adds entries on the scale of minutes |
| entries, continuation page | `cursor_page_ttl_secs` (60 s) | page *n* is only coherent shortly after page *n−1* |
| `unavailable` | `unavailable_ttl_secs` (60 s) | private lists can be made public |
| any error | never stored | an outage is never remembered as unavailability |

Keys include id, `hl`, `gl`, token, limit and the client fingerprint; cursors carry the same dimensions inside their
signed payload.

## 8. Verification status

| Claim | Status | Evidence |
| --- | --- | --- |
| `VL{id}` returns a playlist page | **live-verified** | probe run 43570063308: the uploads playlist of a real channel returned a 4.07 MB page that mapped to 30 entries, `item_count = 550` |
| a live playlist page renders readable entry rows | **live-verified** | same run: 30 entries, all from lockup rows |
| a live playlist page states a page cursor | **no** — the sampled page carried none (`playlist_token=none`, page 2 `no_page_cursor`) | same run: paging a playlist beyond its first page needs a request Vydra does not yet make |
| header/title/item-count/sidebar statistics | **fixture-verified** | `browse_playlist_page.json`, `browse_playlist_page2.json` |
| `playlistVideoRenderer` entries with `index`, `isPlayable`, `lengthSeconds` | **fixture-verified only** | `browse_playlist_page.json`, `browse_playlist_page2.json`; no live page from this egress rendered that shape, so the *classic* row is not verified live |
| lockup entries map with `position: None`, `availability: Unknown` | **live-verified** | probe run 43570063308: 30 entries, `positions=[None, …]`, `unavailable=30` — the page states neither a position nor a playability |
| the same on a synthetic page | **fixture-verified** | `browse_playlist_entries_lockup.json` |
| private/missing alerts become `Unavailable` content facts | **fixture-verified** | `browse_playlist_private_alert.json`, `browse_playlist_missing_alert.json` |
| positions survive continuation pages | **fixture-verified** | page-2 fixture starts at position 5 |
| cursor minting, tamper detection, expiry, fingerprints | **unit-tested** | `vydra-common`, `vydra-engines` |

Two consequences follow from that table, and they are the honest limits of this phase:

1. **A playlist's first page is all Vydra can currently serve live.** The sampled page carried no page cursor, so
   `/playlists/{id}/videos` returns entries and `next_cursor: null`. The code path for continuation pages is
   implemented and unit-tested (and a *channel* continuation page is live-verified), but no live playlist continuation
   has been observed.
2. **`position` and `availability` are `None`/`Unknown` on every live entry**, because the live rows are lockups.
   A client must read `position` as "unknown" rather than "1, 2, 3". This is the ADR-023 decision, now confirmed
   against upstream rather than inferred.

## 9. Reproducing the checks

```bash
cargo test -p vydra-providers --test playlist_parser
cargo test -p vydra-providers --test fixture_provenance
cargo test -p vydra-engines playlist
```

Live evidence requires egress to `www.youtube.com` and is produced by `examples/probe_browse.rs`, which requests a
fixed set of public documents and republishes a bounded summary as CI annotations.
