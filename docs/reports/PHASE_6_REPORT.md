# Phase 6 report — Channel and playlist engines

Branch `arena/06c8b7fc-vydra`, PR #1 (open, **not merged**). Baseline `73c93f1`. Work is backend-only: no frontend,
Android, UI or unrelated infrastructure was touched.

## 1. What was asked, and what was done

| Requirement | Where it lives |
| --- | --- |
| independent provider-neutral `ChannelDocument` | `crates/vydra-domain/src/channel.rs` |
| independent provider-neutral `PlaylistDocument` + `PlaylistItem` | `crates/vydra-domain/src/playlist.rs` |
| typed request/response structs, request building separate from mapping | `crates/vydra-providers/src/innertube/request.rs`, `browse_map.rs` |
| four endpoints with validated ids, opaque cursors and provenance | `crates/vydra-api/src/lib.rs`, `docs/api/openapi.yaml` |
| per-resource cache policy, no Redis, no new dependencies | `crates/vydra-cache`, `crates/vydra-config`, `crates/vydra-engines` |
| pagination that resists cursor confusion and never loops | `vydra-common` (signed envelope), `parse::find_page_cursor`, engine cursor namespaces |
| fixtures with declared provenance | `crates/vydra-providers/tests/fixtures/{synthetic,malformed}`, `tests/fixture_provenance.rs` |
| research contracts | `docs/research/CHANNEL_CONTRACT.md`, `PLAYLIST_CONTRACT.md`, `CHANNEL_PLAYLIST_FIXTURE_PROVENANCE.md` |

Four endpoints, all `GET`, all read-only:

* `/api/v1/channels/{id}`
* `/api/v1/channels/{id}/videos`
* `/api/v1/playlists/{id}`
* `/api/v1/playlists/{id}/videos`

## 2. Baseline

* branch `arena/06c8b7fc-vydra`, clean tree at the start of the phase, HEAD `73c93f1`;
* `docs/PROGRESS.md`, the Phase 4.3, 5A and 5B reports and `docs/architecture/PROVIDER_ENGINE.md` re-read before
  starting; the provider/engine/registry/cache/API layers audited for the seams a channel and playlist engine needs;
* 22 CI jobs green at the baseline (Phase 5B run 43556583369 / 43556586674).

## 3. Domain model

`ChannelDocument`: `summary` (id, title, handle, description, thumbnails, subscriber count, video count),
`availability`, `banner`, `links`, `joined_at`, `country`, `total_view_count`, `tabs`, `verification`, `evidence`,
`provenance`. `PlaylistDocument`: `summary` (id, title, owner, description, item count, thumbnails), `availability`,
`items`, `next_cursor`, `view_count`, `last_updated_text`, `repeated_entries`, `evidence`, `provenance`.

The four rules the model enforces, each with a test:

1. absence is absence — `subscriber_count: None` is not zero and survives serialization (`absent_counts_are_omitted…`);
2. precision travels with the value — `21.4M subscribers` is `{21_400_000, approximate, raw}`; an exact count has no
   `raw` (`an_approximate_count_keeps_its_rendered_text`);
3. a position is upstream's, never Vydra's — `PlaylistItem::position` is `None` when the row does not state one, and is
   never computed from the page offset (`an_entry_without_a_position_omits_it`);
4. `Unknown` availability is not `Available` — a row that does not state `isPlayable` stays unknown and
   `is_playable()` returns false (`an_unknown_availability_is_not_playable_after_a_round_trip`).

## 4. Provider work

* **channel page** — one `browse` call with the canonical `UC…` id. The mapper reads `channelMetadataRenderer`,
  the page header (`pageHeaderRenderer > content > pageHeaderViewModel`, legacy `c4TabbedHeaderRenderer`), the tab
  strip and the alerts. A page with no header and no alert is a `Parse` error, not an empty channel.
* **channel videos, page 1** — the tab `params` blob is *relayed* from the channel page
  (`tabRenderer.endpoint.browseEndpoint.params`) and validated by `is_relayable_params`; no tab parameter table exists
  in the tree. The listing then maps `videoRenderer`, `gridVideoRenderer` and `lockupViewModel` rows.
* **playlist document** — `browseId: "VL" + id`, the only locally concatenated wire string. Header, sidebar and
  metadata facts with `evidence.fallbacks` recording which one won.
* **playlist entries** — `playlistVideoRenderer` (with `index`, `isPlayable`, `lengthSeconds`) and `lockupViewModel`
  (no position, no playability: emitted as `position: None`, `availability: Unknown`, `source: lockup`).
* **pagination** — only `continuationItemRenderer.continuationEndpoint.continuationCommand.token` can become a next
  cursor. Filter chips and sort menus also carry `continuationCommand` tokens; `parse::find_page_cursor` refuses them,
  and a self-repeating token ends the walk with a warning.
* **errors** — `NotFound → 404`, `Unavailable → 409`, `Unsupported → 503`, `Timeout → 504`; a browse response that
  fails to parse is `provider_parse` and is never cached.

## 5. Engine, cursors and cache

Cursors are signed envelopes (`v`, `k`, `p`, `t`, `iat`) minted under a kind namespace
(`channel.videos:{id}` / `playlist.videos:{id}`). A cursor minted for one namespace, provider or fingerprint does not
verify in another; expired cursors are a typed `InvalidRequest` (400), not a silent restart. Cache keys carry id,
`hl`, `gl`, page token, limit and the client fingerprint.

| Resource | TTL |
| --- | --- |
| channel document (`available`) | `metadata_ttl_secs` = 300 s |
| channel document (`unavailable`) | `unavailable_ttl_secs` = 60 s |
| channel document (`unknown`) | never |
| channel videos, page 1 / playlist document | `listing_ttl_secs` = 120 s |
| continuation pages | `cursor_page_ttl_secs` = 60 s |
| any error | never stored |

New configuration keys: `cache.listing_ttl_secs`, `cache.cursor_page_ttl_secs` (both validated, both defaulted, both
documented in `docs/architecture/PROVIDER_ENGINE.md` §6.4).

## 6. API

* ids are validated before any provider call (`ChannelId`/`PlaylistId`); a malformed id is a 400 with the offending
  field and issue, and a guessed id is never treated as a validated resource;
* locales are validated (`hl`/`gl` shapes) before they reach a cache key;
* `@handle` is refused with `400 validation_failed{id: handle_not_supported}` — see §7.3;
* responses are enveloped with `meta` (request id, provenance, cache flag, attempts), never with an API key, a signed
  URL or an internal token;
* OpenAPI: four new paths, three new parameters, twelve new schemas, `npx @redocly/cli lint` valid with the five
  pre-existing warnings and zero errors.

## 7. Evidence and verification boundaries

### 7.1 Live-verified (probe runs 43561178733, 43561555917)

* a channel page parses end to end: id, 7 tabs in document order, `subscribers = 21 400 000` (approximate),
  `videos = 550`, zero parse warnings, evidence sections `channelMetadataRenderer`, `header`, `tabs`;
* the relayed videos-tab `params` blob is 20 bytes and is accepted; the tab response carried a 136-byte page cursor;
* current channel listing rows are `richItemRenderer > content > lockupViewModel`;
* a `@handle` is **not** a browse id: `HTTP 400`.

### 7.2 Fixture-verified (synthetic, sidecar-classed)

Tab order and slugs, ambiguous single-count handling, alerts as content facts, `videoRenderer` listings with a page
cursor, sort-token refusal, truncated JSON failure, playlist header/sidebar facts, entries with positions and
playability, position preservation on page 2, lockup rows (title, thumbnails, duration badge, live badge, invalid id).

### 7.3 Blocked, and why

* **live playlist rows** — the one uploads playlist the probe requested returned no page cursor and no readable entry
  rows. The mapper handles both known row shapes, but no live playlist page with rows has been observed from this
  egress, so the claim stops at fixture-verified.
* **raw captures as fixtures** — the redacted channel page was 7.6 MB (above the probe's inline cap) and the job
  artifact is unreachable from the build sandbox, so no `recorded_vydra_probe` browse fixture exists. See
  `docs/research/CHANNEL_PLAYLIST_FIXTURE_PROVENANCE.md` §5–6.
* **handles** — upstream refuses them (`400`), and resolving one would need a second, guessing request. Refused.

### 7.4 Not implemented (deliberate scope)

Channel `about` tab (links, country, join date, total views), channel playlists/podcasts/releases tabs, playlist
sorting/filtering (would need unverified `params`), comments, captions, live API, and any UI.

## 8. Tests

| Crate | Tests |
| --- | --- |
| `vydra-providers` | 169 |
| `vydra-engines` | 17 |
| `vydra-domain` | 12 |
| `vydra-common` | 26 |
| `vydra-provider-registry` | 13 |
| `vydra-api` | 9 |
| `vydra-config`, `vydra-cache`, `vydra-auth`, `vydra-errors`, `vydra-observability` | 8 / 5 / 5 / 4 / 2 |
| **total** | **270** |

The CI run above executed all 270; the per-crate counts are the attribute count (`#[test]` + `#[tokio::test]`) in
each crate, which is what the 14 test jobs run.

New for Phase 6: 12 channel mapper tests, 7 playlist mapper tests, 6 engine browse tests, 6 domain serialization
tests, 2 probe-workflow-side grep checks (annotations), plus the 12 fixture-provenance checks that now cover 12 browse
fixtures.

Not unit-tested, stated rather than implied: the live path itself (needs egress), and the `VYDRA_PROBE_*` example
(it is a diagnostic, not a test).

## 9. CI

Final state: **22/22 jobs green** on run [43569267185](https://github.com/TienHeavenVN/vydra/actions/runs/43569267185)
(`push`, HEAD `78655a01`) and [43569274250](https://github.com/TienHeavenVN/vydra/actions/runs/43569274250)
(`pull_request`, same SHA). 14 of the 22 jobs are per-crate test jobs; the rest are `fmt`, `check`, `clippy`,
`docker`, `audit`, `deny`, `secrets`, `openapi`.

What the phase cost to get there, recorded because it is the useful part:

| Round | Failures | Cause |
| --- | --- | --- |
| 1 | fmt, clippy, docker, test (providers) | unused parameter in the probe; tab order reversed by a LIFO stack; a dev-dependency added without regenerating `Cargo.lock` |
| 2 | fmt, clippy, docker, test (api/api-server/providers) | `&Error` vs `Error`; unused import; an assertion on `ParseReport::count()`, which counts findings and not warnings |
| 3 | fmt, test (providers) | rustfmt wants `let x = <chain>` broken differently; a synthetic sidecar note must say "not a capture" |
| 4–9 | fmt only | six more rounds of rustfmt preferences (chain width, macro argument width, array breaking) — no compiler or test failures. Rounds 4–9 established that hand-predicting rustfmt without a local toolchain costs ~8 minutes per diff, so a temporary `fmt-apply` job committed rustfmt's own output (`fec1c22`) and was retired in the next commit |

Two CI changes were needed to make these diagnosable at all: the `fmt` job now republishes the rustfmt diff as
annotations, and the `browse` probe job republishes its `PROBE` lines as annotations. Job logs are not downloadable
from the build sandbox, which is why both exist.

## 10. Files changed

51 files, +7260/−33 against `73c93f1`:

* domain: `channel.rs`, `playlist.rs`, `common.rs`, `lib.rs`;
* providers: `innertube/{browse_map,request,provider,mod,map}.rs`, `parse.rs`, `lib.rs`, `examples/probe_browse.rs`,
  `tests/{channel_parser,playlist_parser}.rs`, 12 fixtures + 12 sidecars;
* engines: `lib.rs`; config: `lib.rs`; cache: `lib.rs`; api: `lib.rs`; `apps/api-server/src/main.rs`;
* CI: `.github/workflows/{ci,probe}.yml`, `.github/probe-trigger`;
* docs: `docs/api/openapi.yaml`, `docs/research/{CHANNEL_CONTRACT,PLAYLIST_CONTRACT,CHANNEL_PLAYLIST_FIXTURE_PROVENANCE}.md`,
  `docs/PROGRESS.md`, `docs/architecture/PROVIDER_ENGINE.md`, `docs/research/IMPLEMENTATION_DECISIONS.md` (ADR-023,
  ADR-024), this report.

## 11. Unresolved findings and limitations

1. **Lockup rows are the current shape, and only channel tabs were sampled.** If a playlist surface nests them
   differently, the mapper's by-key fallback may find nothing and the page will report `playlist_entries_missing`
   rather than fabricating entries. That is the failure mode chosen deliberately, but it is a failure mode.
2. **No live playlist listing.** Unchanged from §7.3.
3. **Counts remain positional.** A page that renders subscribers and videos in the other order in some locale would
   swap them; the ambiguity guard only catches the one-part case. Mitigation: both values are `approximate` and keep
   their rendered text, so a swap is visible in `raw`.
4. **`item_count` is not reconciled with `items.len()`.** Private/deleted entries occupy positions, so the two
   legitimately differ; clients that compute "missing" from the difference will be wrong.
5. **Probe artifacts are unreachable.** Any future capture-to-fixture promotion needs an environment that can download
   workflow artifacts.
6. **`about`-tab facts are absent by design**, so `links`, `country`, `joined_at` and `total_view_count` always
   serialize as absent — a client must not read that as "the channel states nothing".

## 12. Next recommendation

1. Promote one live channel page and one live playlist page to `recorded_vydra_probe` fixtures from an environment
   that can reach the artifacts, then flip the two lockup mappers from fixture-verified to live-verified.
2. Only then add the channel `playlists`, `shorts` and `live` tabs — they reuse everything Phase 6 built (relayed
   `params`, cursors, cache classes) and add no new request shapes.
3. Keep handles refused until a handle→id resolution can be verified as an exact-match lookup rather than a search
   guess.
