# Security model

## Defaults

| Control | Default | Notes |
|---|---|---|
| API key required | `false` (dev) / mandatory in production | `Config::validate` refuses to start in production without it |
| Bootstrap key | absent | refused in production |
| Cursor secret | dev literal | production refuses the default value; minimum 16 bytes |
| CORS | disabled | explicit allowlist only; `*` refused in production |
| Request body limit | 64 KiB | enforced by `RequestBodyLimitLayer` |
| Request timeout | 10 s | edge `TimeoutLayer` + per-provider deadline |
| Provider response cap | 4 MiB | enforced while streaming, not after buffering |
| Outbound scheme | HTTPS only | `reqwest` builder `https_only(true)` |
| Outbound host allowlist | YouTube/Google hosts | `transport::is_allowed_host`; redirects are not followed |
| Logs | no secrets | `SecretString` redaction + `is_sensitive_header` |
| SQL | no string interpolation | `sqlx` bind parameters only |

## Threat model (abbreviated)

| Threat | Mitigation |
|---|---|
| SSRF via crafted ids | identifiers are newtypes with strict charsets; provider URLs are built from validated ids only |
| Provider JSON injecting URLs | domain models have no URL fields for media; thumbnails are normalized and host-checked |
| Resource exhaustion | body caps, response caps, parse node/depth budgets, registry attempt cap |
| Credential leak in logs/errors | secret wrapper, redaction helper, public errors carry codes not messages |
| Enumeration of internal state | cursors are HMAC-signed; `/status` exposes sanitized health only |
| Replay of pagination cursors | cursors expire (default 24 h) and are bound to resource kind + provider |
| Timing oracle on API keys | constant-time comparison |
| Unbounded fan-out | per-provider attempt cap (≤3) and registry-enforced deadlines |

## Known gaps (not yet done)

- API keys are verified only from config/bootstrap in v1; the Postgres repository exists but is not
  wired into the edge yet.
- Redis-backed distributed rate limiting and quota are not implemented.
- No penetration test, no fuzzing campaign beyond property tests, no load test.
- Supply chain: `cargo-deny` + `rustsec/audit-check` are configured but have never run.
