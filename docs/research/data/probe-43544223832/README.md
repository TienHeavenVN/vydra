# Raw probe capture — run 43544223832

The verbatim, redacted responses that `crates/vydra-providers/examples/probe_search.rs` received on
2026-11-14 through `ReqwestTransport`, gzipped (the fixtures reduced from them are in
`crates/vydra-providers/tests/fixtures/recorded/search_web_probe_*`).

| file | HTTP | bytes (uncompressed) | request body keys |
|---|---|---|---|
| `search_page_1.json.gz` | 200 | 660 981 | `context`, `query` |
| `search_page_2.json.gz` | 200 | 488 298 | `context`, `continuation` |
| `search_mixed.json.gz` | 200 | 1 236 535 | `context`, `query` (`veritasium`) |
| `search_empty.json.gz` | 200 | 21 754 | `context`, `query` |
| `search_error.json.gz` | 400 | 274 | `context` only |
| `summary.json.gz` | — | — | what the probe recorded about each capture |

Why they are here: an artifact expires (14 days), and a fixture whose source no longer exists is a
claim, not evidence. They are gzipped because the JSON is 10× larger; nothing else is modified.

Redaction was applied by the probe before writing (`examples/probe_search.rs`): the API key, visitor
id, cookie material and every URL query string are removed, as are `webPageUrl`/`canonicalBaseUrl`
values. They contain public, unauthenticated search results — nothing account-specific.

Reproduce the fixtures from them:

```
mkdir -p /tmp/probe-out
for f in docs/research/data/probe-43544223832/*.json.gz; do
  gunzip -c "$f" > "/tmp/probe-out/$(basename "$f" .gz)"
done
python3 scripts/fixtures/import_probe.py /tmp/probe-out crates/vydra-providers/tests/fixtures \
    --capture-run https://github.com/TienHeavenVN/vydra/actions/runs/43544223832
python3 scripts/fixtures/inventory_renderers.py /tmp/probe-out    # the per-key inventory
```
