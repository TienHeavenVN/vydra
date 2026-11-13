//! PostgreSQL access for durable state (API keys, worker leases).

#![forbid(unsafe_code)]

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use vydra_config::DatabaseConfig;
use vydra_errors::Error;

/// Optional database handle: Vydra runs without Postgres in development, but production policy
/// requires it (see `Config::validate`).
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Connect when a URL is configured; returns `Ok(None)` otherwise.
    pub async fn connect(config: &DatabaseConfig) -> Result<Option<Self>, Error> {
        let Some(url) = config.url.as_ref() else {
            tracing::warn!("database.url is not set: durable state is disabled");
            return Ok(None);
        };
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections.clamp(1, 100))
            .acquire_timeout(std::time::Duration::from_millis(config.acquire_timeout_ms))
            .connect(url.expose())
            .await
            .map_err(|err| Error::Database(format!("connect failed: {err}")))?;
        Ok(Some(Self { pool }))
    }

    /// Apply embedded migrations.
    pub async fn migrate(&self) -> Result<(), Error> {
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await
            .map_err(|err| Error::Database(format!("migration failed: {err}")))
    }

    pub async fn healthy(&self) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }

    /// Look up an API key by prefix.
    pub async fn find_api_key(
        &self,
        prefix: &str,
    ) -> Result<Option<ApiKeyRow>, Error> {
        let row = sqlx::query_as::<_, ApiKeyRow>(
            "SELECT id::text, prefix, secret_hash, monthly_quota, revoked_at IS NOT NULL AS revoked \
             FROM api_keys WHERE prefix = $1 LIMIT 1",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| Error::Database(format!("api key lookup failed: {err}")))?;
        Ok(row)
    }
}

/// Row shape returned by the API-key lookup.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub prefix: String,
    pub secret_hash: Vec<u8>,
    pub monthly_quota: i64,
    pub revoked: bool,
}
