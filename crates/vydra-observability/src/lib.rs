//! Observability: tracing, Prometheus metrics and log redaction.
//!
//! Audit finding A-08: the previous revision created metric vectors with `let _ = …` and never
//! registered them, so `/metrics` exposed only a build counter. Every vector below is owned by the
//! struct and registered with the registry at construction.

#![forbid(unsafe_code)]

use std::time::Duration;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};
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

/// Metric handles, all registered in one registry.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    provider_requests: IntCounterVec,
    provider_latency: HistogramVec,
    circuit_state: IntGaugeVec,
    http_requests: IntCounterVec,
    quota_denied: IntCounterVec,
    build_info: IntGaugeVec,
}

impl Metrics {
    pub fn new() -> Result<Self, ObservabilityError> {
        let map_err = |err: prometheus::Error| ObservabilityError::Metrics(err.to_string());

        let provider_requests = IntCounterVec::new(
            Opts::new(
                "vydra_provider_requests_total",
                "Provider attempts by provider, capability and outcome.",
            ),
            &["provider", "capability", "outcome"],
        )
        .map_err(map_err)?;

        let provider_latency = HistogramVec::new(
            HistogramOpts::new(
                "vydra_provider_latency_seconds",
                "Latency of provider calls in seconds.",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["provider", "capability"],
        )
        .map_err(map_err)?;

        let circuit_state = IntGaugeVec::new(
            Opts::new(
                "vydra_provider_circuit_state",
                "Circuit breaker state: 0 closed, 1 half-open, 2 open.",
            ),
            &["provider"],
        )
        .map_err(map_err)?;

        let http_requests = IntCounterVec::new(
            Opts::new("vydra_http_requests_total", "HTTP requests served."),
            &["route", "method", "status"],
        )
        .map_err(map_err)?;

        let quota_denied = IntCounterVec::new(
            Opts::new(
                "vydra_quota_denied_total",
                "Requests denied by quota or rate limiting.",
            ),
            &["reason"],
        )
        .map_err(map_err)?;

        let build_info = IntGaugeVec::new(
            Opts::new("vydra_build_info", "Static build information."),
            &["version"],
        )
        .map_err(map_err)?;

        let registry = Registry::new();
        for collector in [
            Box::new(provider_requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(provider_latency.clone()),
            Box::new(circuit_state.clone()),
            Box::new(http_requests.clone()),
            Box::new(quota_denied.clone()),
            Box::new(build_info.clone()),
        ] {
            registry.register(collector).map_err(map_err)?;
        }

        Ok(Self {
            registry,
            provider_requests,
            provider_latency,
            circuit_state,
            http_requests,
            quota_denied,
            build_info,
        })
    }

    pub fn set_build_info(&self, version: &str) {
        self.build_info.with_label_values(&[version]).set(1);
    }

    pub fn record_provider_attempt(
        &self,
        provider: &str,
        capability: &str,
        outcome: &str,
        latency: Duration,
    ) {
        self.provider_requests
            .with_label_values(&[provider, capability, outcome])
            .inc();
        self.provider_latency
            .with_label_values(&[provider, capability])
            .observe(latency.as_secs_f64());
    }

    pub fn record_circuit_state(&self, provider: &str, state: &str) {
        let value = match state {
            "closed" => 0,
            "half_open" => 1,
            _ => 2,
        };
        self.circuit_state.with_label_values(&[provider]).set(value);
    }

    pub fn record_http(&self, route: &str, method: &str, status: u16) {
        self.http_requests
            .with_label_values(&[route, method, &status.to_string()])
            .inc();
    }

    pub fn record_quota_denied(&self, reason: &str) {
        self.quota_denied.with_label_values(&[reason]).inc();
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

/// Redact a header value for logging.
pub fn redacted_header_value(name: &str, value: &str) -> String {
    if is_sensitive_header(name) {
        "<redacted>".to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_registered_and_render() {
        let metrics = Metrics::new().expect("metrics");
        metrics.set_build_info("0.1.0");
        metrics.record_provider_attempt(
            "innertube-web",
            "video_metadata",
            "ok",
            Duration::from_millis(12),
        );
        metrics.record_circuit_state("innertube-web", "open");
        metrics.record_http("/api/v1/search", "GET", 200);
        metrics.record_quota_denied("rate_limited");

        let text = metrics.encode().expect("encode");
        assert!(text.contains("vydra_provider_requests_total"));
        assert!(text.contains("vydra_provider_latency_seconds"));
        assert!(text.contains("vydra_provider_circuit_state"));
        assert!(text.contains("vydra_http_requests_total"));
        assert!(text.contains("vydra_quota_denied_total"));
        assert!(text.contains("vydra_build_info"));
        assert!(text.contains("provider=\"innertube-web\""));
    }

    #[test]
    fn sensitive_headers_are_redacted() {
        assert_eq!(
            redacted_header_value("Authorization", "Bearer x"),
            "<redacted>"
        );
        assert_eq!(
            redacted_header_value("X-Goog-Visitor-Id", "abc"),
            "<redacted>"
        );
        assert_eq!(
            redacted_header_value("Accept", "application/json"),
            "application/json"
        );
    }
}
