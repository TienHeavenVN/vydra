# Channel and playlist fixture provenance

Every channel/playlist test in this repository is driven by a fixture whose sidecar says exactly what it is. This
document is the human-readable companion to the machine-checked rules in
`crates/vydra-providers/tests/fixture_provenance.rs`.

## 1. Classes

| Class | Meaning | Used by Phase 6 |
| --- | --- | --- |
| `recorded_vydra_probe` | a byte-for-byte copy of a `browse` response Vydra's own probe received, after the documented reduction in §4 | **none** (see §5) |
| `recorded_upstream_subset` | a genuine response reduced to the subtree a test needs, with the reduction recorded | none |
| `synthetic` | hand-built; every value is a placeholder | all ten browse fixtures, plus two lockup fixtures |
| `derived_malformed` | a synthetic or recorded fixture with one documented, reversible mutation | `malformed/browse_truncated.json` |

The sidecar fields are `class`, `provenance`, `note`, `endpoint`, `created`, `purpose`, `sections_exercised` and
`not_verified`. `not_verified` is mandatory for `synthetic` fixtures: a fixture that does not say what it fails to
prove is a rumour with JSON syntax.

## 2. The corpus

| Fixture | Class | What it exercises |
| --- | --- | --- |
| `synthetic/browse_channel_page.json` | synthetic | `channelMetadataRenderer`, `pageHeaderRenderer`/`pageHeaderViewModel`, avatar, banner, two count parts, seven tabs, verification badge |
| `synthetic/browse_channel_page_ambiguous_counts.json` | synthetic | a single count part: both counts stay absent and the ambiguity is a warning |
| `synthetic/browse_channel_missing_alert.json` | synthetic | a channel page that is an `ERROR` alert, not a document |
| `synthetic/browse_channel_videos_page1.json` | synthetic | `videoRenderer` rows, a genuine page cursor, a sort chip carrying a token, a duplicated row |
| `synthetic/browse_channel_videos_page2.json` | synthetic | a continuation page (no channel re-statement) |
| `synthetic/browse_channel_videos_sort_only.json` | synthetic | a page whose only token is a sort token: no `next_cursor` |
| `synthetic/browse_channel_videos_lockup.json` | synthetic, **keyed like a live sample** | `richItemRenderer > content > lockupViewModel` rows: duration badge, live badge, metadata row pair, an invalid `contentId`, a page cursor |
| `synthetic/browse_playlist_page.json` | synthetic | `playlistHeaderRenderer` header, `playlistVideoRenderer` entries with `index`/`isPlayable`/`lengthSeconds` |
| `synthetic/browse_playlist_page2.json` | synthetic | a continuation page whose first entry sits at position 5 |
| `synthetic/browse_playlist_entries_lockup.json` | synthetic, **keyed like a live sample** | lockup entries: no `index`, no `isPlayable` |
| `synthetic/browse_playlist_private_alert.json` | synthetic | a private list rendered as an alert |
| `synthetic/browse_playlist_missing_alert.json` | synthetic | a missing list rendered as an alert |
| `malformed/browse_truncated.json` | derived_malformed | a truncated JSON document: the parser fails closed with `provider_parse` |

`synthetic/browse_channel_videos_lockup.json` and `synthetic/browse_playlist_entries_lockup.json` deserve a special
note: their **key structure** is not invented. It was copied from the shape sample that the Phase 6 probe printed for
a live channel Videos tab (§5), which is why the production mapper and the fixture agree on where the title,
thumbnails, badges and metadata rows live. Their **values** are all placeholders. A test may therefore claim "the
mapper reads this shape correctly"; it may not claim "upstream serves this shape on a playlist page" — the sidecar
`not_verified` says so explicitly.

## 3. What a test may claim with these fixtures

* **Yes**: the mapper reads a documented shape, refuses to guess, keeps positions, counts duplicates, refuses chip
  tokens, fails closed on truncated JSON, records evidence sections.
* **No**: that upstream returns that shape today, on every surface, in every locale. Only §5 upgrades a claim to
  live-verified, and only for the fields the probe actually reported.

## 4. Reduction applied to any capture before it could be committed

The probe redacts before writing (see `examples/probe_browse.rs`): tracking params, session identifiers, signed URL
query strings, engagement-panel and download machinery, and every field the mapper never reads are dropped or
replaced. A capture is also bounded: if a redacted capture exceeds the probe's size cap, only its FNV-1a checksum
(64-bit, non-cryptographic, for matching a log line to an artifact) and its shape inventory are published.

## 5. The live evidence this phase actually has

Three probe runs were made on 2026-11-13 (branch `arena/06c8b7fc-vydra`):

| Run | Workflow | Result | What it established |
| --- | --- | --- | --- |
| 43560646681 | `probe` | success | the browse probe runs from a branch at all (`push` on `.github/probe-trigger`, because `workflow_dispatch` is not dispatchable while the workflow is absent from the default branch) |
| 43561178733 | `probe` | success | a genuine channel page parsed: `id`, 7 tabs, `subscribers=21 400 000`, `videos=550`, zero parse warnings; the videos tab accepted the relayed 20-byte `params` blob and returned a 136-byte page cursor; `browseId: "@veritasium"` returned `HTTP 400` |
| 43561555917 | `probe` | success | the same, plus the **row shape**: channel Videos rows are `richItemRenderer > content > lockupViewModel`; the page carries 30 `lockupViewModel`, 3 `continuationItemRenderer` and 1 `chipBarViewModel`, and the uploads playlist page returned neither a page cursor nor readable entry rows |

**No raw capture is committed for Phase 6.** The redacted channel page was 7.6 MB, above the probe's inline-dump cap,
and the job artifact is not reachable from this environment (the same limitation that makes job logs undownloadable).
What is committed instead is the *finding*: the shape inventory, quoted in the fixture sidecars and in
`docs/research/CHANNEL_CONTRACT.md` §9. Anyone with egress can regenerate the capture with the command in §7 and
promote it to a `recorded_vydra_probe` fixture under the rules above; until then the honest label for the lockup
mappers is "fixture-verified against a sampled shape", not "live-verified".

## 6. Why the corpus has no recorded channel or playlist fixture

Two independent reasons, both worth writing down instead of papering over:

1. size — a channel page is megabytes even after redaction, and fixtures are committed to git;
2. reachability — the probe's artifact upload is not retrievable from the build sandbox, so a capture that the probe
   *did* make cannot be moved into `tests/fixtures/` by the agent that made it.

The corpus therefore keeps the pre-Phase-6 policy (synthetic fixtures plus explicit sidecars) rather than pretending
to have captures it does not have, and `docs/reports/PHASE_6_REPORT.md` §7 states which claims that costs.

## 7. Regeneration

```bash
# live capture (needs egress to www.youtube.com; the development sandbox has none)
VYDRA_PROBE_CHANNEL_ID=UCHnyfMqiRRG1u-2MsSQLbXA \
VYDRA_PROBE_CHANNEL_HANDLE=@veritasium \
VYDRA_PROBE_OUT=probe-out/ \
cargo run --release -p vydra-providers --example probe_browse

# archive a redacted capture (gz + manifest with SHA-256) into the corpus directory
python3 scripts/fixtures/archive_probe.py probe-out/ docs/research/data/browse --kind browse \
    --source-class live

# fixture hygiene, offline
cargo test -p vydra-providers --test fixture_provenance
```
