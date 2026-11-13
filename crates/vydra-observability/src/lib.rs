//! Observability: tracing, Prometheus metrics and log redaction.

#![forbid(unsafe_code)]

use prometheus::{Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder};
use tracing_subscriber::EnvFilter;
use vydra_config::{LogFormat, ObservabilityConfig};

/// Errors raised while initialising observability.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    #[error("could not build tracing subscriber: {0}")]
    Tracing(String),
    #[error("could not register metrics: {0}")]
    Metrics(String),
    #[error("could not encode metrics: {0}")]
    Encode(String),
}

/// Initialise structured logging. JSON in production, pretty locally.
pub fn init_tracing(config: &ObservabilityConfig) -> Result<(), ObservabilityError> {
    let filter = EnvFilter::try_new(&config.log_filter)
        .map_err(|err| ObservabilityError::Tracing(err.to_string()))?;
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match config.log_format {
        LogFormat::Json => builder
            .json()
            .with_current_span(true)
            .try_init()
            .map_err(|err| ObservabilityError::Tracing(err.to_string())),
        LogFormat::Pretty => builder
            .try_init()
            .map_err(|err| ObservabilityError::Tracing(err.to_string())),
    }
}

/// Metric handles. Cheap to clone through `Arc`.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    pkg: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Result<Self, ObservabilityError> {
        let _ = HistogramVec::new(
            HistogramOpts::new(
                "vydra_provider_latency_seconds",
                "Latency of provider calls in seconds.",
            )
            .buckets(vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["provider", "capability"],
        )
        .map_err(|err| ObservabilityError::Metrics(err.to_string()))?;
        let _ = IntCounterVec::new(
            Opts::new("vydra_http_requests_total", "HTTP requests served."),
            &["route", "method", "status"],
        )
        .map_err(|err| ObservabilityError::Metrics(err.to_string()))?;
        let _ = IntCounterVec::new(
            Opts::new(
                "vydra_quota_denied_total",
                "Requests denied by quota or rate limiting.",
            ),
            &["reason"],
        )
        .map_err(|err| ObservabilityError::Metrics(err.to_string()))?;
        Ok(Self {
            registry: Registry::new(),
            pkg: IntCounterVec::new(
                Opts::new("vydra_build_info", "Static build information."),
                &["version"],
            )
            .map_err(|err| ObservabilityError::Metrics(err.to_string()))?,
        })
    }

    pub fn set_build_info(&self, version: &str) {
        self.pkg.with_label_values(&[version]).inc();
    }

    /// Render Prometheus text exposition format.
    pub fn encode(&self) -> Result<String, ObservabilityError> {
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder
            .encode(&self.registry.gather(), &mut buffer)
            .map_err(|err| ObservabilityError::Encode(err.to_string()))?;
        String::from_utf8(buffer).map_err(|err| ObservabilityError::Encode(err.to_string()))
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

/// Headers whose values must never be logged.
pub fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "authorization" | "cookie" | "set-cookie" | "x-api-key" | "proxy-authorization"
    ) || lower.starts_with("x-goog-")
}
