# Fixture corpus

Three classes, never mixed. Every file has a sibling `*.meta.json` that states where it came from
and what was removed from it.

| Directory | Class | What it proves | What it does **not** prove |
| --- | --- | --- | --- |
| `synthetic/` | hand-built, shaped after documented schema | the parser handles the shapes Vydra *expects*, including cases no capture in the corpus covers (private video, live, malformed fields) | nothing about YouTube's real output |
| `recorded/` | subset of a payload recorded by a third-party test suite | Vydra parses **real** upstream JSON, with its real localization, real ids and real renderer nesting | nothing about Vydra's *request* shape, and nothing about today's YouTube |
| `malformed/` | derived from `recorded/` by deletion or truncation | the parser and the transport fail closed on incomplete or corrupt input | nothing about upstream |

Rules:

1. **Nothing here was captured by Vydra.** The sandbox that produced this repository cannot reach
   YouTube; no live request has ever been made from Vydra. `recorded/` files are subsets of payloads
   recorded by `yuliskov/MediaServiceCore` (MIT) and are attributed in their meta files.
2. **Recorded values are never rewritten.** Reduction means deleting keys and list elements. If a
   value had to change to be committable (a URL query string carrying a signature), the whole query
   string is removed and the meta file says so.
3. **Synthetic fixtures say so in their first line.** `search_mixed.json` is explicitly
   `synthetic_schema_reference`; it is not a capture and must never be described as one.
4. **No credential or personal data**: `responseContext` (visitor id, API key echo), `trackingParams`
   and all URL query strings are removed by `scripts/fixtures/extract_recorded.py`.
5. `scripts/fixtures/extract_recorded.py` regenerates `recorded/` and `malformed/` from an upstream
   checkout; see `docs/research/FIXTURE_PROVENANCE.md` for the exact command and commit.
