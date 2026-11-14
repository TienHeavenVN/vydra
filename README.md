# Vydra

A backend-only, Rust-first **YouTube data API** platform: one consistent REST surface over pluggable
extraction providers, with typed models, honest provenance, bounded fallback and no fabricated data.

> **Status: Phases 1–4 (research, architecture, foundation, provider engine core) are implemented in
> this tree. Nothing here has been compiled or executed yet — this environment has no Rust toolchain
> and no access to `crates.io`.** Read [`docs/PROGRESS.md`](docs/PROGRESS.md) before trusting any
> claim, including the endpoint table.

## What exists

| Area | State |
|---|---|
| Research (12 repositories, source-level) | done — `docs/research/` |
| Architecture + provider contract + API contract | done — `docs/architecture/` |
| Workspace, config, errors, domain models, common parsers | written, **not compiled** |
| InnerTube provider (search, player, captions, chapters) | written, **not compiled**, not live-verified |
| Provider registry (health, circuit breaker, bounded fallback) | written, **not compiled** |
| HTTP edge (envelope, error contract, request id, auth, limits) | written, **not compiled** |
| Postgres/Redis, comments, playlist, channel, live, discovery | not implemented |

## Design in one paragraph

Providers declare capabilities; the registry routes a request only to providers that declared the
required capability, tracks per-provider health with a circuit breaker, and falls back **only** on
transient failures (`Timeout`, `RateLimited`, `Upstream`, `Transport`, `Parse`) — never on content
facts such as "private" or "deleted". Providers return provider-neutral `vydra-domain` types; raw
provider JSON never reaches the API. Every document carries `provenance` (provider, endpoint,
client, fetch time, cache state, parse-warning count) so a client can tell exact data from
approximate data. Pagination cursors are opaque and HMAC-signed, so provider continuation tokens are
never exposed. Media URLs that would require circumventing integrity/anti-bot controls are **not**
implemented (ADR-006) — format metadata is served instead, with the reason a URL is absent.

## Quick start

```bash
cp .env.example .env 2>/dev/null || true
cargo run -p vydra-api-server      # requires a Rust toolchain ≥ 1.88
curl localhost:8080/api/v1/health
curl localhost:8080/api/v1/status
curl 'localhost:8080/api/v1/search?q=rust&limit=5'
```

Docker: `docker compose up --build`.

## Documentation

- `docs/research/` — how the ecosystem actually works (with file-level citations)
- `docs/architecture/` — architecture, provider interface, API contracts
- `docs/api/openapi.yaml` — OpenAPI 3.1 skeleton (validated in CI)
- `docs/security/SECURITY.md` — threat model and security defaults
- `docs/providers/MEDIA_LIMITATIONS.md` — why media URLs are deliberately out of scope
- `docs/PROGRESS.md` — what is verified, what is not, and what is blocked

## License

MIT OR Apache-2.0. The reference projects studied in `docs/research/` are **not** vendored: copyleft
references were used for design study only (see `docs/research/IMPLEMENTATION_DECISIONS.md`, ADR-011).
