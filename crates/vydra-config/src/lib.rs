//! Layered, validated configuration.
//!
//! Precedence: built-in defaults → environment variables (prefix `VYDRA__`, `__` between segments).
//! Secrets are wrapped in [`SecretString`] so that they cannot be accidentally logged via `Debug`,
//! `Display` or a config dump (ADR-014).
//!
//! Rationale for env-only (no file layer yet): the reference projects show that a config *file*
//! becomes a deployment artifact that drifts. Container-first deployments (ADR-015) prefer env.
//! A file layer can be added without changing the public getters.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vydra_common::MIN_SECRET_LEN;
use vydra_errors::{Error, FieldIssue, Result};

/// Runtime environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Development,
    Staging,
    Production,
}

impl Environment {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" | "local" => Some(Self::Development),
            "staging" | "stage" => Some(Self::Staging),
            "production" | "prod" => Some(Self::Production),
            _ => None,
        }
    }

    pub fn is_production(self) -> bool {
        matches!(self, Self::Production)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
}

/// A secret that refuses to render itself.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Access the secret. Call sites should be few and auditable.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString(<redacted, {} bytes>)", self.0.len())
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// HTTP server configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Deadline applied to every request at the edge.
    pub request_timeout_ms: u64,
    /// Maximum accepted request body (bytes).
    pub body_limit_bytes: usize,
    /// Graceful shutdown budget.
    pub shutdown_grace_ms: u64,
    /// Explicit CORS allowlist; empty means CORS is disabled entirely (secure default).
    pub cors_allowed_origins: Vec<String>,
    /// Whether `/metrics` and `/status` are exposed on the public listener.
    pub expose_diagnostics: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 8080,
            request_timeout_ms: 10_000,
            body_limit_bytes: 64 * 1024,
            shutdown_grace_ms: 15_000,
            cors_allowed_origins: Vec::new(),
            expose_diagnostics: true,
        }
    }
}

/// Provider engine configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub innertube: InnertubeConfig,
    /// Maximum provider attempts per request (bounded fallback, ADR-007).
    pub max_attempts: u8,
    /// Default content language sent to providers.
    pub default_hl: String,
    /// Default content country sent to providers.
    pub default_gl: String,
    /// Opt-in for search filters (sort/since/duration/features). The mapping to InnerTube `sp=`
    /// parameters is **unverified** (no live-network confirmation is possible from the build
    /// sandbox), so the filters are refused unless an operator explicitly enables them.
    pub enable_unverified_filters: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            innertube: InnertubeConfig::default(),
            max_attempts: 2,
            default_hl: "en".into(),
            default_gl: "US".into(),
            enable_unverified_filters: false,
        }
    }
}

/// InnerTube provider configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InnertubeConfig {
    pub enabled: bool,
    /// Client profile used by default (`web`, `mweb`, `tv`).
    pub default_client: String,
    /// Per-request timeout for a provider call.
    pub request_timeout_ms: u64,
    /// TTL for cached session/bootstrap data.
    pub session_ttl_secs: u64,
    /// Hard cap on an upstream response body.
    pub max_response_bytes: usize,
    /// Overrides the bootstrap api key (mainly for tests and experiments).
    pub api_key_override: Option<SecretString>,
    /// Consent cookie value, e.g. `YES+cb`. `None` sends no consent cookie.
    pub consent_cookie: Option<String>,
    /// Operator overrides for client versions (`web=2.20260101.00.00`).
    pub client_version_overrides: HashMap<String, String>,
}

impl Default for InnertubeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_client: "web".into(),
            request_timeout_ms: 6_000,
            session_ttl_secs: 6 * 60 * 60,
            max_response_bytes: 4 * 1024 * 1024,
            api_key_override: None,
            consent_cookie: None,
            client_version_overrides: HashMap::new(),
        }
    }
}

/// Cache backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBackend {
    /// In-process only. Fragments across replicas; fine for single-node deployments.
    Memory,
    /// Redis. Required for multi-replica quota correctness.
    Redis,
}

/// Cache configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    pub redis_url: Option<SecretString>,
    /// Maximum entries in the in-memory cache.
    pub max_entries: usize,
    pub metadata_ttl_secs: u64,
    pub search_ttl_secs: u64,
    /// Serve stale entries for this long while revalidating. `0` disables stale serving.
    pub stale_while_revalidate_secs: u64,
    /// Whether search responses are cached at all (off by default: highest churn).
    pub cache_search: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory,
            redis_url: None,
            max_entries: 10_000,
            metadata_ttl_secs: 300,
            search_ttl_secs: 60,
            stale_while_revalidate_secs: 0,
            cache_search: false,
        }
    }
}

/// Database (PostgreSQL) configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub url: Option<SecretString>,
    pub max_connections: u32,
    pub acquire_timeout_ms: u64,
    pub run_migrations: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: None,
            max_connections: 10,
            acquire_timeout_ms: 3_000,
            run_migrations: true,
        }
    }
}

/// API key authentication and quota configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// When false, requests are admitted without a key (development only; refused in production).
    pub required: bool,
    /// Local development bootstrap key. Startup warns and it is refused in production.
    pub bootstrap_key: Option<SecretString>,
    /// Requests per minute per key.
    pub per_key_rpm: u32,
    /// Requests per minute per client IP (applies even when keys are optional).
    pub per_ip_rpm: u32,
    /// Monthly request quota per key. `0` means unlimited.
    pub monthly_quota: u64,
    /// HMAC secret for pagination cursors.
    pub cursor_secret: SecretString,
    /// Cursor lifetime.
    pub cursor_ttl_secs: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            required: false,
            bootstrap_key: None,
            per_key_rpm: 60,
            per_ip_rpm: 120,
            monthly_quota: 0,
            cursor_secret: SecretString::new("vydra-dev-cursor-secret-change-me"),
            cursor_ttl_secs: 24 * 60 * 60,
        }
    }
}

/// Log format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Observability configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// `tracing` filter directive, e.g. `info,vydra_providers=debug`.
    pub log_filter: String,
    pub log_format: LogFormat,
    pub metrics_enabled: bool,
    /// OTLP endpoint. `None` disables export (structured logs + Prometheus remain).
    pub otel_endpoint: Option<String>,
    pub service_name: String,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_filter: "info,vydra_api=debug,vydra_providers=debug".into(),
            log_format: LogFormat::Pretty,
            metrics_enabled: true,
            otel_endpoint: None,
            service_name: "vydra".into(),
        }
    }
}

/// Root configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub environment: Environment,
    pub server: ServerConfig,
    pub providers: ProvidersConfig,
    pub cache: CacheConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub observability: ObservabilityConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            environment: Environment::Development,
            server: ServerConfig::default(),
            providers: ProvidersConfig::default(),
            cache: CacheConfig::default(),
            database: DatabaseConfig::default(),
            auth: AuthConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

/// A collected set of configuration problems, so startup reports all of them at once.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid configuration: {}", .0.iter().map(|i| format!("{}:{}", i.field, i.issue)).collect::<Vec<_>>().join(", "))]
pub struct ConfigError(pub Vec<FieldIssue>);

impl ConfigError {
    fn push(mut self, field: &str, issue: &str) -> Self {
        self.0.push(FieldIssue::new(field, issue));
        self
    }

    pub fn into_error(self) -> Error {
        Error::Validation(self.0)
    }
}

impl Config {
    /// Load configuration from the process environment.
    pub fn from_env() -> Result<Self> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        let config = Self::from_map(&vars)?;
        config.validate().map_err(ConfigError::into_error)?;
        Ok(config)
    }

    /// Load configuration from an arbitrary map (unit-testable).
    pub fn from_map(vars: &HashMap<String, String>) -> Result<Self> {
        let mut config = Config::default();
        let mut issues: Vec<FieldIssue> = Vec::new();

        if let Some(value) = vars.get("VYDRA__ENV") {
            match Environment::parse(value) {
                Some(env) => config.environment = env,
                None => issues.push(FieldIssue::new("env", "invalid_value")),
            }
        }

        // --- server ---
        set_string(vars, "VYDRA__SERVER__HOST", &mut config.server.host);
        set_u16(vars, "VYDRA__SERVER__PORT", &mut config.server.port, "server.port", &mut issues);
        set_u64(
            vars,
            "VYDRA__SERVER__REQUEST_TIMEOUT_MS",
            &mut config.server.request_timeout_ms,
            "server.request_timeout_ms",
            &mut issues,
        );
        set_usize(
            vars,
            "VYDRA__SERVER__BODY_LIMIT_BYTES",
            &mut config.server.body_limit_bytes,
            "server.body_limit_bytes",
            &mut issues,
        );
        set_u64(
            vars,
            "VYDRA__SERVER__SHUTDOWN_GRACE_MS",
            &mut config.server.shutdown_grace_ms,
            "server.shutdown_grace_ms",
            &mut issues,
        );
        if let Some(value) = vars.get("VYDRA__SERVER__CORS_ALLOWED_ORIGINS") {
            config.server.cors_allowed_origins = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        set_bool_opt(
            vars,
            "VYDRA__SERVER__EXPOSE_DIAGNOSTICS",
            &mut config.server.expose_diagnostics,
        );

        // --- providers ---
        set_bool_opt(
            vars,
            "VYDRA__PROVIDERS__INNERTUBE__ENABLED",
            &mut config.providers.innertube.enabled,
        );
        set_string(
            vars,
            "VYDRA__PROVIDERS__INNERTUBE__DEFAULT_CLIENT",
            &mut config.providers.innertube.default_client,
        );
        set_u64(
            vars,
            "VYDRA__PROVIDERS__INNERTUBE__REQUEST_TIMEOUT_MS",
            &mut config.providers.innertube.request_timeout_ms,
            "providers.innertube.request_timeout_ms",
            &mut issues,
        );
        set_u64(
            vars,
            "VYDRA__PROVIDERS__INNERTUBE__SESSION_TTL_SECS",
            &mut config.providers.innertube.session_ttl_secs,
            "providers.innertube.session_ttl_secs",
            &mut issues,
        );
        set_usize(
            vars,
            "VYDRA__PROVIDERS__INNERTUBE__MAX_RESPONSE_BYTES",
            &mut config.providers.innertube.max_response_bytes,
            "providers.innertube.max_response_bytes",
            &mut issues,
        );
        if let Some(value) = vars.get("VYDRA__PROVIDERS__INNERTUBE__API_KEY") {
            config.providers.innertube.api_key_override = Some(SecretString::new(value.clone()));
        }
        if let Some(value) = vars.get("VYDRA__PROVIDERS__INNERTUBE__CONSENT_COOKIE") {
            config.providers.innertube.consent_cookie = Some(value.clone());
        }
        set_u8(
            vars,
            "VYDRA__PROVIDERS__MAX_ATTEMPTS",
            &mut config.providers.max_attempts,
            "providers.max_attempts",
            &mut issues,
        );
        set_string(vars, "VYDRA__PROVIDERS__DEFAULT_HL", &mut config.providers.default_hl);
        set_string(vars, "VYDRA__PROVIDERS__DEFAULT_GL", &mut config.providers.default_gl);
        set_bool_opt(
            vars,
            "VYDRA__PROVIDERS__ENABLE_UNVERIFIED_FILTERS",
            &mut config.providers.enable_unverified_filters,
        );

        // --- cache ---
        if let Some(value) = vars.get("VYDRA__CACHE__BACKEND") {
            config.cache.backend = match value.trim().to_ascii_lowercase().as_str() {
                "memory" => CacheBackend::Memory,
                "redis" => CacheBackend::Redis,
                _ => {
                    issues.push(FieldIssue::new("cache.backend", "invalid_value"));
                    CacheBackend::Memory
                }
            };
        }
        if let Some(value) = vars.get("VYDRA__CACHE__REDIS_URL") {
            config.cache.redis_url = Some(SecretString::new(value.clone()));
        }
        set_usize(
            vars,
            "VYDRA__CACHE__MAX_ENTRIES",
            &mut config.cache.max_entries,
            "cache.max_entries",
            &mut issues,
        );
        set_u64(
            vars,
            "VYDRA__CACHE__METADATA_TTL_SECS",
            &mut config.cache.metadata_ttl_secs,
            "cache.metadata_ttl_secs",
            &mut issues,
        );
        set_u64(
            vars,
            "VYDRA__CACHE__SEARCH_TTL_SECS",
            &mut config.cache.search_ttl_secs,
            "cache.search_ttl_secs",
            &mut issues,
        );
        set_bool_opt(vars, "VYDRA__CACHE__CACHE_SEARCH", &mut config.cache.cache_search);

        // --- database ---
        if let Some(value) = vars.get("VYDRA__DATABASE__URL") {
            config.database.url = Some(SecretString::new(value.clone()));
        }
        set_u32(
            vars,
            "VYDRA__DATABASE__MAX_CONNECTIONS",
            &mut config.database.max_connections,
            "database.max_connections",
            &mut issues,
        );
        set_bool_opt(
            vars,
            "VYDRA__DATABASE__RUN_MIGRATIONS",
            &mut config.database.run_migrations,
        );

        // --- auth ---
        set_bool_opt(vars, "VYDRA__AUTH__REQUIRED", &mut config.auth.required);
        if let Some(value) = vars.get("VYDRA__AUTH__BOOTSTRAP_KEY") {
            config.auth.bootstrap_key = Some(SecretString::new(value.clone()));
        }
        set_u32(
            vars,
            "VYDRA__AUTH__PER_KEY_RPM",
            &mut config.auth.per_key_rpm,
            "auth.per_key_rpm",
            &mut issues,
        );
        set_u32(
            vars,
            "VYDRA__AUTH__PER_IP_RPM",
            &mut config.auth.per_ip_rpm,
            "auth.per_ip_rpm",
            &mut issues,
        );
        if let Some(value) = vars.get("VYDRA__AUTH__CURSOR_SECRET") {
            config.auth.cursor_secret = SecretString::new(value.clone());
        }
        set_u64(
            vars,
            "VYDRA__AUTH__CURSOR_TTL_SECS",
            &mut config.auth.cursor_ttl_secs,
            "auth.cursor_ttl_secs",
            &mut issues,
        );

        // --- observability ---
        set_string(
            vars,
            "VYDRA__OBSERVABILITY__LOG_FILTER",
            &mut config.observability.log_filter,
        );
        if let Some(value) = vars.get("VYDRA__OBSERVABILITY__LOG_FORMAT") {
            config.observability.log_format = match value.trim().to_ascii_lowercase().as_str() {
                "json" => LogFormat::Json,
                "pretty" => LogFormat::Pretty,
                _ => {
                    issues.push(FieldIssue::new("observability.log_format", "invalid_value"));
                    LogFormat::Pretty
                }
            };
        }
        set_bool_opt(
            vars,
            "VYDRA__OBSERVABILITY__METRICS_ENABLED",
            &mut config.observability.metrics_enabled,
        );
        if let Some(value) = vars.get("VYDRA__OBSERVABILITY__OTEL_ENDPOINT") {
            config.observability.otel_endpoint = Some(value.clone());
        }
        set_string(
            vars,
            "VYDRA__OBSERVABILITY__SERVICE_NAME",
            &mut config.observability.service_name,
        );

        if issues.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError(issues).into_error())
        }
    }

    /// Validate cross-field invariants and environment-specific requirements.
    pub fn validate(&self) -> std::result::Result<(), ConfigError> {
        let mut err = ConfigError(Vec::new());

        if self.server.port == 0 {
            err = err.push("server.port", "out_of_range");
        }
        if self.server.request_timeout_ms < 100 {
            err = err.push("server.request_timeout_ms", "too_small");
        }
        if self.server.body_limit_bytes < 1024 {
            err = err.push("server.body_limit_bytes", "too_small");
        }
        if !(1..=3).contains(&self.providers.max_attempts) {
            err = err.push("providers.max_attempts", "out_of_range");
        }
        if self.providers.innertube.request_timeout_ms < 100 {
            err = err.push("providers.innertube.request_timeout_ms", "too_small");
        }
        if self.providers.innertube.max_response_bytes < 64 * 1024 {
            err = err.push("providers.innertube.max_response_bytes", "too_small");
        }
        if self.providers.default_hl.trim().is_empty() || self.providers.default_hl.len() > 16 {
            err = err.push("providers.default_hl", "invalid_value");
        }
        if self.providers.default_gl.len() != 2 || !self.providers.default_gl.chars().all(|c| c.is_ascii_alphabetic()) {
            err = err.push("providers.default_gl", "invalid_value");
        }
        if self.cache.backend == CacheBackend::Redis && self.cache.redis_url.is_none() {
            err = err.push("cache.redis_url", "required");
        }
        if self.cache.max_entries == 0 {
            err = err.push("cache.max_entries", "out_of_range");
        }
        if self.auth.cursor_secret.len() < MIN_SECRET_LEN {
            err = err.push("auth.cursor_secret", "too_short");
        }
        if self.auth.per_key_rpm == 0 {
            err = err.push("auth.per_key_rpm", "out_of_range");
        }
        if self.auth.per_ip_rpm == 0 {
            err = err.push("auth.per_ip_rpm", "out_of_range");
        }

        // Production-specific security requirements.
        if self.environment.is_production() {
            if !self.auth.required {
                err = err.push("auth.required", "required_in_production");
            }
            if self.auth.bootstrap_key.is_some() {
                err = err.push("auth.bootstrap_key", "forbidden_in_production");
            }
            if self.auth.cursor_secret.expose() == "vydra-dev-cursor-secret-change-me" {
                err = err.push("auth.cursor_secret", "default_value_in_production");
            }
            if self.database.url.is_none() {
                err = err.push("database.url", "required_in_production");
            }
            if !self.server.cors_allowed_origins.is_empty()
                && self
                    .server
                    .cors_allowed_origins
                    .iter()
                    .any(|origin| origin == "*")
            {
                err = err.push("server.cors_allowed_origins", "wildcard_in_production");
            }
        }

        if err.0.is_empty() { Ok(()) } else { Err(err) }
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.server.request_timeout_ms)
    }

    pub fn provider_timeout(&self) -> Duration {
        Duration::from_millis(self.providers.innertube.request_timeout_ms)
    }

    /// Log-safe summary of the effective configuration.
    pub fn redacted_summary(&self) -> String {
        format!(
            "env={} listen={}:{} providers(innertube={},client={},attempts={}) cache={:?} db={} auth_required={} otel={}",
            self.environment.as_str(),
            self.server.host,
            self.server.port,
            self.providers.innertube.enabled,
            self.providers.innertube.default_client,
            self.providers.max_attempts,
            self.cache.backend,
            self.database.url.is_some(),
            self.auth.required,
            self.observability.otel_endpoint.is_some(),
        )
    }

    /// Path used by the worker for a lease file, if any.
    pub fn data_dir(&self) -> PathBuf {
        std::env::var_os("VYDRA__DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/vydra"))
    }
}

// --- small typed env helpers -------------------------------------------------------------

fn set_string(vars: &HashMap<String, String>, key: &str, target: &mut String) {
    if let Some(value) = vars.get(key) {
        if !value.trim().is_empty() {
            *target = value.trim().to_owned();
        }
    }
}

fn set_bool_opt(vars: &HashMap<String, String>, key: &str, target: &mut bool) {
    if let Some(value) = vars.get(key) {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => *target = true,
            "0" | "false" | "no" | "off" => *target = false,
            _ => {}
        }
    }
}

macro_rules! set_numeric {
    ($fn_name:ident, $ty:ty, $label:literal) => {
        fn $fn_name(
            vars: &HashMap<String, String>,
            key: &str,
            target: &mut $ty,
            field: &str,
            issues: &mut Vec<FieldIssue>,
        ) {
            if let Some(value) = vars.get(key) {
                match value.trim().parse::<$ty>() {
                    Ok(parsed) => *target = parsed,
                    Err(_) => issues.push(FieldIssue::new(field, "invalid_value")),
                }
            }
        }
    };
}

set_numeric!(set_u8, u8, "u8");
set_numeric!(set_u16, u16, "u16");
set_numeric!(set_u32, u32, "u32");
set_numeric!(set_u64, u64, "u64");
set_numeric!(set_usize, usize, "usize");

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_validate() {
        Config::default().validate().expect("defaults are valid");
    }

    #[test]
    fn env_overrides_apply() {
        let vars = map(&[
            ("VYDRA__ENV", "staging"),
            ("VYDRA__SERVER__PORT", "9090"),
            ("VYDRA__PROVIDERS__DEFAULT_GL", "VN"),
            ("VYDRA__PROVIDERS__INNERTUBE__ENABLED", "false"),
            ("VYDRA__CACHE__BACKEND", "redis"),
            ("VYDRA__CACHE__REDIS_URL", "redis://localhost:6379"),
        ]);
        let config = Config::from_map(&vars).expect("parse");
        config.validate().expect("valid");
        assert_eq!(config.environment, Environment::Staging);
        assert_eq!(config.server.port, 9090);
        assert_eq!(config.providers.default_gl, "VN");
        assert!(!config.providers.innertube.enabled);
        assert_eq!(config.cache.backend, CacheBackend::Redis);
    }

    #[test]
    fn invalid_numbers_are_reported() {
        let vars = map(&[("VYDRA__SERVER__PORT", "not-a-port")]);
        let err = Config::from_map(&vars).expect_err("must fail");
        match err {
            Error::Validation(issues) => assert_eq!(issues[0].field, "server.port"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn redis_requires_url() {
        let vars = map(&[("VYDRA__CACHE__BACKEND", "redis")]);
        let config = Config::from_map(&vars).expect("parse");
        let err = config.validate().expect_err("must fail");
        assert!(err.0.iter().any(|i| i.field == "cache.redis_url"));
    }

    #[test]
    fn production_requires_auth_and_rejects_bootstrap_key() {
        let vars = map(&[
            ("VYDRA__ENV", "production"),
            ("VYDRA__AUTH__BOOTSTRAP_KEY", "vydra_dev_secret"),
            ("VYDRA__DATABASE__URL", "postgres://localhost/vydra"),
            ("VYDRA__AUTH__CURSOR_SECRET", "a-sufficiently-long-production-secret"),
        ]);
        let config = Config::from_map(&vars).expect("parse");
        let err = config.validate().expect_err("must fail");
        let fields: Vec<&str> = err.0.iter().map(|i| i.field.as_str()).collect();
        assert!(fields.contains(&"auth.required"));
        assert!(fields.contains(&"auth.bootstrap_key"));
    }

    #[test]
    fn secrets_are_redacted_in_debug_and_display() {
        let secret = SecretString::new("super-secret-value");
        assert!(!format!("{secret:?}").contains("super-secret-value"));
        assert!(!format!("{secret}").contains("super-secret-value"));
        assert!(!format!("{secret:?}").contains("super-secret-value"));
    }

    #[test]
    fn redacted_summary_hides_secrets() {
        let vars = map(&[
            ("VYDRA__DATABASE__URL", "postgres://user:password@db/vydra"),
            ("VYDRA__AUTH__CURSOR_SECRET", "another-production-grade-secret"),
        ]);
        let config = Config::from_map(&vars).expect("parse");
        let summary = config.redacted_summary();
        assert!(!summary.contains("password"));
        assert!(!summary.contains("another-production-grade-secret"));
    }

    #[test]
    fn cursor_secret_length_is_enforced() {
        let vars = map(&[("VYDRA__AUTH__CURSOR_SECRET", "short")]);
        let config = Config::from_map(&vars).expect("parse");
        let err = config.validate().expect_err("must fail");
        assert!(err.0.iter().any(|i| i.field == "auth.cursor_secret"));
    }
}
