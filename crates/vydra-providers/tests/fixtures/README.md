# Fixture corpus

Three classes, never mixed. Every file has a sibling `*.meta.json` that states where it came from
and what was removed from it.

| Directory | Class | What it proves | What it does **not** prove |
| --- | --- | --- | --- |
| `synthetic/` | hand-built, shaped after documented schema | the parser handles the shapes Vydra *expects*, including cases no capture in the corpus covers (private video, live, malformed fields) | nothing about YouTube's real output |
| `recorded/` | **either** a subset of a payload recorded by a third-party test suite **or** a response Vydra captured itself | Vydra parses **real** upstream JSON, with its real localization, real ids and real renderer nesting | nothing about today's YouTube, and — for third-party files — nothing about Vydra's request shape |
| `malformed/` | derived from `recorded/` by deletion or truncation | the parser and the transport fail closed on incomplete or corrupt input | nothing about upstream |

Rules:

1. **Every file states its class in its `*.meta.json`.** Since Phase 4.3 some `recorded/` files are
   `recorded_vydra_probe`: responses Vydra itself received through its production transport, on a stated
   date and run id. The rest are `recorded_upstream_subset` files from `yuliskov/MediaServiceCore`
   (MIT). Neither class may be described as "what upstream returns today", and the class table in
   `docs/research/FIXTURE_PROVENANCE.md` (plus the player appendix,
   `docs/research/PLAYER_FIXTURE_PROVENANCE.md`) is authoritative.
2. **Recorded values are never rewritten.** Reduction means deleting keys and list elements. If a
   value had to change to be committable (a URL query string carrying a signature), the whole query
   string is removed and the meta file says so.
3. **Synthetic fixtures say so in their first line.** `search_mixed.json` is explicitly
   `synthetic_schema_reference`; it is not a capture and must never be described as one.
4. **No credential or personal data**: `responseContext` (visitor id, API key echo), `trackingParams`
   and all URL query strings are removed by `scripts/fixtures/extract_recorded.py`.
5. `scripts/fixtures/extract_recorded.py` regenerates `recorded/` and `malformed/` from an upstream
   checkout; see `docs/research/FIXTURE_PROVENANCE.md` for the exact command and commit.

Rules 1, 3 and 4 are enforced by `tests/fixture_provenance.rs` (Phase 5B): every fixture needs a sidecar,
the sidecar's `class` must come from the closed set, the directory must agree with the class, and a
synthetic file must say it is not a capture and list what it does not prove. Rule 2 (values are never
rewritten) is a review rule — no test can prove it.
