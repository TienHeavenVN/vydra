# Comments fixture provenance

Every comment test in this repository is driven by a fixture whose sidecar says exactly what it is. This document
is the human-readable companion to the machine-checked rules in
`crates/vydra-providers/tests/fixture_provenance.rs`.

## 1. Classes

| Class | Meaning | Used by Phase 7 |
| --- | --- | --- |
| `recorded_vydra_probe` | a copy of a `next` response Vydra's own probe received, after the documented reduction in §4 | **none committed** (see §5) |
| `recorded_upstream_subset` | a genuine response reduced to the subtree a test needs, with the reduction recorded | none |
| `synthetic` | hand-built; every value is a placeholder | all six comment fixtures |
| `derived_malformed` | a synthetic or recorded fixture with one documented, reversible mutation | none (hostile rows are separate synthetic files instead) |

The sidecar fields are `class`, `provenance`, `note`, `endpoint`, `created`, `purpose`, `sections_exercised` and
`not_verified`. `not_verified` is mandatory for `synthetic` fixtures: a fixture that does not say what it fails to
prove is a rumour with JSON syntax.

## 2. The corpus

| Fixture | Class | What it exercises |
| --- | --- | --- |
| `synthetic/comments_watch_page_with_section.json` | synthetic | a watch page whose content list carries `itemSectionRenderer.targetId == "comments-section"` and its `continuationItemRenderer` token: the only entry point to a comment section |
| `synthetic/comments_watch_page_without_section.json` | synthetic | a watch page with no comment section: the shape that must be read as comments-disabled, never as an empty list |
| `synthetic/comments_page1_top.json` | synthetic | one page carrying **both** renderer shapes at once (view model + `frameworkUpdates` entity join, and the classic `commentRenderer`), a pinned and hearted comment, a thread with one loaded reply plus a reply continuation, an abbreviated like count, the sort menu and the next-page cursor |
| `synthetic/comments_page1_newest.json` | synthetic | the same page with the second menu entry selected: what a page fetched with the `newest` token looks like |
| `synthetic/comments_replies_page1.json` | synthetic | a reply page with both reply row shapes (bare `commentViewModel`, bare `commentRenderer`) and a reply continuation |
| `synthetic/comments_page1_malformed_rows.json` | synthetic | an unknown renderer, a thread with no body, an entity with no comment id, an invalid comment id, a duplicated row, and two continuation items on one page |

Two fixtures deserve a note:

* `comments_page1_top.json` deliberately mixes the renderer generation **within one page**. That is not a
  fabrication to make a test easy: it is the shape that forces the mapper to decide per row, and the reason the
  mapper has four entry combinations rather than one. Its *values* are all placeholders, and its sidecar
  `not_verified` says that no capture backs the mixture.
* `comments_page1_malformed_rows.json` is filed under `synthetic/` rather than `malformed/` because it is valid
  JSON with hostile *content*: `malformed/` in this corpus is for documents that are not JSON, which is a different
  failure mode (`provider_parse`).

## 3. What a test may claim with these fixtures

* **Yes**: the mapper reads both renderer shapes, joins the entity batch update, keeps the heart and pinned flags,
  preserves the localized publication label, distinguishes an absent count from a zero one, stamps reply parents,
  refuses an order the menu does not offer, counts duplicates and hostile rows, and refuses to derive an author id
  from a display name.
* **No**: that upstream serves these shapes today, on every surface, in every locale; that the sort menu has exactly
  two entries in this order; that a reply page is always delivered through `onResponseReceivedEndpoints`; that
  duplicates occur on real pages. Only §5 upgrades a claim to live-verified, and only for what the probe reported.

## 4. Reduction applied to any capture before it could be committed

`examples/probe_comments.rs` redacts before writing:

* `responseContext`, `trackingParams`, `clickTrackingParams`, `serviceTrackingParams`, `loggingContext`,
  ad placements, `visitorData`, `apiKey` and consent material are dropped;
* every URL keeps its path and loses its query string (that is where signed parameters live);
* **comment text and author display names are replaced by `<redacted len=N>`**. A comment section is a list of
  private individuals' prose and names; the length is evidence about shape and the words are not Vydra's to publish.
  A redacted capture is therefore *not* a byte-for-byte copy of what upstream sent, and its sidecar must say so;
* a capture that exceeds the probe's size cap is published as a checksum (FNV-1a 64-bit, non-cryptographic, for
  matching a log line to an artifact) plus a shape inventory, not as a body;
* a refused request is recorded as a refusal. No body is written for it and nothing is invented to fill the gap.

## 5. The live evidence this phase actually has

See `docs/reports/PHASE_7_REPORT.md` §7 for the current state. The rules for this corpus:

1. a `recorded_vydra_probe` comment fixture may only be added from a probe run whose id and date are recorded in the
   sidecar, after the §4 reduction;
2. the probe artifact expires (14 days), so a committed capture also needs its checksum and its shape inventory in
   this document, and the text redaction must be stated in the sidecar's `note`;
3. no comment fixture may be described as "what YouTube returns" — only as "what one run returned on one date".
