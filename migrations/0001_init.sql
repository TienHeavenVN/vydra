-- Vydra initial schema.
--
-- Durable state only: API keys and worker leases. Metadata documents live in Redis/cache, not here,
-- until there is a measured need for durable storage (ADR-010, ADR-015).

CREATE TABLE IF NOT EXISTS api_keys (
    id              UUID PRIMARY KEY,
    prefix          TEXT        NOT NULL UNIQUE,
    secret_hash     BYTEA       NOT NULL,
    label           TEXT        NOT NULL DEFAULT '',
    scopes          TEXT[]      NOT NULL DEFAULT '{}',
    monthly_quota   BIGINT      NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS api_keys_prefix_idx ON api_keys (prefix);

CREATE TABLE IF NOT EXISTS worker_leases (
    name            TEXT PRIMARY KEY,
    holder          TEXT        NOT NULL,
    acquired_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS usage_counters (
    key_id          UUID        NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    period          TEXT        NOT NULL,          -- e.g. 2026-11
    requests        BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (key_id, period)
);
