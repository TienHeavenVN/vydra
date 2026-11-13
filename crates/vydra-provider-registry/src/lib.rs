//! Provider registry: capability-based selection, health tracking, circuit breaking and **bounded**
//! fallback (ADR-007).
//!
//! Normative behaviour is documented in `docs/architecture/PROVIDER_INTERFACE.md` §5. The single most
//! important rule: a deterministic content fact (`Unavailable`, `NotFound`) is returned immediately
//! and never triggers another provider call.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use vydra_domain::{AttemptOutcome, SourceAttempt};
use vydra_errors::{Error, ProviderError};
use vydra_providers::{
    Capability, CapabilitySet, Provider, ProviderContext, ProviderId, ProviderRequest,
    ProviderResponse, SharedProvider,
};

/// Registry tuning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    /// Maximum provider attempts per request (hard-capped at 3).
    pub max_attempts: u8,
    /// Sliding window size used to compute the failure rate.
    pub window: usize,
    /// Minimum samples before the breaker may open.
    pub min_samples: usize,
    /// Failure ratio (percent) that opens the breaker.
    pub open_threshold_percent: u8,
    /// Base cooldown; doubles on each consecutive open up to `max_cooldown`.
    pub base_cooldown: Duration,
    pub max_cooldown: Duration,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            window: 30,
            min_samples: 10,
            open_threshold_percent: 50,
            base_cooldown: Duration::from_secs(5),
            max_cooldown: Duration::from_secs(300),
        }
    }
}

/// Circuit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open { until: Instant },
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open { .. } => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

#[derive(Debug)]
struct ProviderState {
    window: Vec<bool>,
    consecutive_failures: usize,
    consecutive_opens: u32,
    state: CircuitState,
    probe_in_flight: bool,
    last_error: Option<String>,
    total_ok: u64,
    total_failed: u64,
}

impl ProviderState {
    fn new() -> Self {
        Self {
            window: Vec::new(),
            consecutive_failures: 0,
            consecutive_opens: 0,
            state: CircuitState::Closed,
            probe_in_flight: false,
            last_error: None,
            total_ok: 0,
            total_failed: 0,
        }
    }

    fn success_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 1.0;
        }
        let ok = self.window.iter().filter(|ok| **ok).count();
        ok as f64 / self.window.len() as f64
    }

    fn record(&mut self, healthy: bool, config: &RegistryConfig, now: Instant) {
        self.window.push(healthy);
        if self.window.len() > config.window {
            self.window.remove(0);
        }
        if healthy {
            self.total_ok += 1;
            self.consecutive_failures = 0;
            if self.state == CircuitState::HalfOpen {
                self.state = CircuitState::Closed;
                self.consecutive_opens = 0;
                self.probe_in_flight = false;
            }
        } else {
            self.total_failed += 1;
            self.consecutive_failures += 1;
        }

        if self.state == CircuitState::Closed
            && self.window.len() >= config.min_samples
            && self.success_rate() * 100.0 < f64::from(100 - config.open_threshold_percent)
        {
            let cooldown = config
                .base_cooldown
                .saturating_mul(1u32 << self.consecutive_opens.min(6))
                .min(config.max_cooldown);
            self.consecutive_opens = self.consecutive_opens.saturating_add(1);
            self.state = CircuitState::Open {
                until: now + cooldown,
            };
        }
    }

    fn refresh(&mut self, now: Instant) {
        if let CircuitState::Open { until } = self.state {
            if now >= until {
                self.state = CircuitState::HalfOpen;
                self.probe_in_flight = false;
            }
        }
    }

    /// Whether a request may be attempted now; consumes the half-open probe slot.
    fn allow(&mut self, now: Instant) -> bool {
        self.refresh(now);
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open { .. } => false,
            CircuitState::HalfOpen => {
                if self.probe_in_flight {
                    false
                } else {
                    self.probe_in_flight = true;
                    true
                }
            }
        }
    }
}

/// Public, sanitized health snapshot (`GET /status`).
#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealthSnapshot {
    pub provider: String,
    pub client: String,
    pub state: String,
    pub success_rate: f32,
    pub samples: usize,
    pub consecutive_failures: usize,
    pub last_error_code: Option<String>,
    pub capabilities: Vec<String>,
}

/// Outcome of a registry fetch, including every attempt (surfaced in `meta.sources`).
#[derive(Debug)]
pub struct FetchOutcome {
    pub response: ProviderResponse,
    pub provider: ProviderId,
    pub attempts: Vec<SourceAttempt>,
}

/// The registry.
#[derive(Debug)]
pub struct ProviderRegistry {
    providers: Vec<SharedProvider>,
    states: Mutex<HashMap<ProviderId, ProviderState>>,
    config: RegistryConfig,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<SharedProvider>) -> Self {
        Self::with_config(providers, RegistryConfig::default())
    }

    pub fn with_config(providers: Vec<SharedProvider>, config: RegistryConfig) -> Self {
        let mut states = HashMap::new();
        for provider in &providers {
            states.insert(provider.id(), ProviderState::new());
        }
        Self {
            providers,
            states: Mutex::new(states),
            config: RegistryConfig {
                max_attempts: config.max_attempts.clamp(1, 3),
                ..config
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Union of all declared capabilities.
    pub fn capabilities(&self) -> CapabilitySet {
        self.providers
            .iter()
            .fold(CapabilitySet::NONE, |acc, provider| acc.union(provider.capabilities()))
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities().contains(capability)
    }

    pub fn provider_ids(&self) -> Vec<ProviderId> {
        self.providers.iter().map(|provider| provider.id()).collect()
    }

    /// Sanitized health for `GET /status`.
    pub fn health(&self) -> Vec<ProviderHealthSnapshot> {
        let now = Instant::now();
        let mut states = match self.states.lock() {
            Ok(states) => states,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.providers
            .iter()
            .map(|provider| {
                let state = states.entry(provider.id()).or_insert_with(ProviderState::new);
                state.refresh(now);
                ProviderHealthSnapshot {
                    provider: provider.id().as_str().to_owned(),
                    client: provider.client_label(),
                    state: state.state.as_str().to_owned(),
                    success_rate: state.success_rate() as f32,
                    samples: state.window.len(),
                    consecutive_failures: state.consecutive_failures,
                    last_error_code: state.last_error.clone(),
                    capabilities: provider
                        .capabilities()
                        .iter()
                        .map(|cap| cap.as_str().to_owned())
                        .collect(),
                }
            })
            .collect()
    }

    fn record(&self, provider: ProviderId, error: Option<&ProviderError>) {
        let now = Instant::now();
        let mut states = match self.states.lock() {
            Ok(states) => states,
            Err(poisoned) => poisoned.into_inner(),
        };
        let state = states.entry(provider).or_insert_with(ProviderState::new);
        match error {
            None => state.record(true, &self.config, now),
            Some(err) if err.affects_health() => {
                state.last_error = Some(err.code().to_owned());
                state.record(false, &self.config, now);
            }
            Some(_) => { /* content facts do not affect health */ }
        }
    }

    fn candidates(&self, capability: Capability) -> Vec<SharedProvider> {
        let now = Instant::now();
        let mut states = match self.states.lock() {
            Ok(states) => states,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut eligible = Vec::new();
        for provider in &self.providers {
            if !provider.capabilities().contains(capability) {
                continue;
            }
            let state = states.entry(provider.id()).or_insert_with(ProviderState::new);
            if state.allow(now) {
                eligible.push(provider.clone());
            } else {
                tracing::debug!(provider = %provider.id(), "skipped: circuit open");
            }
        }
        eligible
    }

    /// Execute a request with bounded fallback.
    pub async fn fetch(
        &self,
        request: ProviderRequest,
        ctx: &ProviderContext,
    ) -> Result<FetchOutcome, Error> {
        let capability = request.required_capability();
        let total_capable = self
            .providers
            .iter()
            .filter(|provider| provider.capabilities().contains(capability))
            .count();

        if total_capable == 0 {
            return Err(Error::CapabilityUnavailable(capability.as_str().to_owned()));
        }

        let candidates = self.candidates(capability);
        if candidates.is_empty() {
            return Err(Error::ProviderCircuitOpen {
                retry_after: Some(self.config.base_cooldown),
            });
        }

        let attempts_limit = usize::from(self.config.max_attempts).min(candidates.len());
        let mut attempts: Vec<SourceAttempt> = Vec::with_capacity(attempts_limit);
        let mut last_transient: Option<ProviderError> = None;

        for (index, provider) in candidates.iter().take(attempts_limit).enumerate() {
            let started = Instant::now();
            let result = tokio::time::timeout(ctx.remaining(), provider.fetch(request.clone(), ctx))
                .await
                .unwrap_or(Err(ProviderError::Timeout));

            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

            match result {
                Ok(response) => {
                    self.record(provider.id(), None);
                    let empty = response.is_empty();
                    attempts.push(SourceAttempt {
                        provider: provider.id().as_str().to_owned(),
                        endpoint: response.endpoint().to_owned(),
                        attempt: index as u8,
                        latency_ms,
                        outcome: if empty {
                            AttemptOutcome::Empty
                        } else {
                            AttemptOutcome::Ok
                        },
                    });
                    tracing::debug!(
                        provider = %provider.id(),
                        capability = capability.as_str(),
                        latency_ms,
                        "provider ok"
                    );
                    return Ok(FetchOutcome {
                        response,
                        provider: provider.id(),
                        attempts,
                    });
                }
                Err(error) => {
                    self.record(provider.id(), Some(&error));
                    attempts.push(SourceAttempt {
                        provider: provider.id().as_str().to_owned(),
                        endpoint: request.endpoint().to_owned(),
                        attempt: index as u8,
                        latency_ms,
                        outcome: AttemptOutcome::Failed,
                    });
                    tracing::warn!(
                        provider = %provider.id(),
                        capability = capability.as_str(),
                        error = error.code(),
                        "provider failed"
                    );

                    if !error.is_fallback_eligible() {
                        // Deterministic: do not spend quota on another provider.
                        return Err(Error::from_provider(error));
                    }
                    last_transient = Some(error);
                }
            }
        }

        match last_transient {
            Some(error) => Err(Error::from_provider(error)),
            None => Err(Error::ProviderUnavailable(
                "no provider attempt completed".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use vydra_common::VideoId;
    use vydra_domain::Provenance;
    use vydra_providers::Strictness;
    use vydra_errors::UnavailabilityReason;

    use super::*;

    fn ctx() -> ProviderContext {
        ProviderContext {
            deadline: Instant::now() + Duration::from_secs(2),
            request_id: "registry-test".into(),
            hl: "en".into(),
            gl: "US".into(),
            strictness: Strictness::Lenient,
        }
    }

    fn request() -> ProviderRequest {
        ProviderRequest::Video {
            id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
            hl: "en".into(),
            gl: "US".into(),
        }
    }

    fn provenance(provider: &str) -> Provenance {
        Provenance {
            provider: provider.into(),
            endpoint: "player".into(),
            client: None,
            fetched_at: time_now(),
            cached: false,
            parse_warnings: 0,
        }
    }

    fn time_now() -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc()
    }

    #[derive(Debug)]
    struct StubProvider {
        id: ProviderId,
        capabilities: CapabilitySet,
        behaviour: Behaviour,
        calls: Arc<AtomicUsize>,
    }

    #[derive(Debug, Clone)]
    enum Behaviour {
        Ok,
        Fail(ProviderError),
        Delay(Duration),
    }

    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> ProviderId {
            self.id
        }
        fn capabilities(&self) -> CapabilitySet {
            self.capabilities
        }
        fn client_label(&self) -> String {
            "stub".into()
        }
        async fn fetch(
            &self,
            _request: ProviderRequest,
            _ctx: &ProviderContext,
        ) -> Result<ProviderResponse, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.behaviour {
                Behaviour::Ok => Ok(ProviderResponse::Video(Box::new(stub_document(&self.id)))),
                Behaviour::Fail(error) => Err(error.clone()),
                Behaviour::Delay(duration) => {
                    tokio::time::sleep(*duration).await;
                    Ok(ProviderResponse::Video(Box::new(stub_document(&self.id))))
                }
            }
        }
    }

    fn stub_document(id: &ProviderId) -> vydra_domain::VideoDocument {
        vydra_domain::VideoDocument {
            summary: vydra_domain::VideoSummary {
                id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
                title: format!("from {}", id),
                channel_id: None,
                channel_title: None,
                duration_seconds: Some(10),
                view_count: None,
                published_text: None,
                published_at: None,
                thumbnails: vec![],
                live_state: None,
                badges: vec![],
            },
            availability: vydra_domain::Availability::Available,
            description: None,
            category: None,
            keywords: vec![],
            like_count: None,
            comment_count: None,
            chapters: vec![],
            caption_tracks: vec![],
            formats: vec![],
            provenance: provenance(id.as_str()),
        }
    }

    fn stub(id: &'static str, behaviour: Behaviour) -> (SharedProvider, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = StubProvider {
            id: ProviderId::new(id),
            capabilities: CapabilitySet::from_slice(&[Capability::VideoMetadata]),
            behaviour,
            calls: calls.clone(),
        };
        (Arc::new(provider), calls)
    }

    #[tokio::test]
    async fn falls_back_to_the_second_provider_on_transient_error() {
        let (first, first_calls) = stub("a", Behaviour::Fail(ProviderError::Timeout));
        let (second, second_calls) = stub("b", Behaviour::Ok);
        let registry = ProviderRegistry::new(vec![first, second]);

        let outcome = registry.fetch(request(), &ctx()).await.expect("ok");
        assert_eq!(outcome.provider.as_str(), "b");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.attempts.len(), 2);
        assert_eq!(outcome.attempts[0].outcome, AttemptOutcome::Failed);
        assert_eq!(outcome.attempts[1].outcome, AttemptOutcome::Ok);
    }

    #[tokio::test]
    async fn does_not_fall_back_on_content_facts() {
        let unavailable = ProviderError::Unavailable {
            reason: UnavailabilityReason::Private,
            message: "private".into(),
        };
        let (first, first_calls) = stub("a", Behaviour::Fail(unavailable));
        let (second, second_calls) = stub("b", Behaviour::Ok);
        let registry = ProviderRegistry::new(vec![first, second]);

        let error = registry.fetch(request(), &ctx()).await.expect_err("must fail");
        assert!(matches!(error, Error::ContentUnavailable { .. }));
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0, "quota must not be spent");
    }

    #[tokio::test]
    async fn attempts_are_bounded() {
        let (a, a_calls) = stub("a", Behaviour::Fail(ProviderError::Timeout));
        let (b, b_calls) = stub("b", Behaviour::Fail(ProviderError::Timeout));
        let (c, c_calls) = stub("c", Behaviour::Ok);
        let registry = ProviderRegistry::with_config(
            vec![a, b, c],
            RegistryConfig {
                max_attempts: 2,
                ..Default::default()
            },
        );
        let error = registry.fetch(request(), &ctx()).await.expect_err("must fail");
        assert!(matches!(error, Error::Provider(_) | Error::ProviderUnavailable(_)));
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 1);
        assert_eq!(c_calls.load(Ordering::SeqCst), 0, "third provider must not be tried");
    }

    #[tokio::test]
    async fn missing_capability_is_reported_not_guessed() {
        let (provider, _) = stub("a", Behaviour::Ok);
        let registry = ProviderRegistry::new(vec![provider]);
        let request = ProviderRequest::Search {
            query: "x".into(),
            kind: vydra_providers::SearchKind::Videos,
            filters: Default::default(),
            cursor: None,
            hl: "en".into(),
            gl: "US".into(),
            limit: 20,
        };
        let error = registry.fetch(request, &ctx()).await.expect_err("must fail");
        assert!(matches!(error, Error::CapabilityUnavailable(_)));
        assert!(!registry.supports(Capability::SearchVideos));
        assert!(registry.supports(Capability::VideoMetadata));
    }

    #[tokio::test]
    async fn circuit_opens_after_repeated_failures_and_stops_calling() {
        let (provider, calls) = stub("a", Behaviour::Fail(ProviderError::Upstream {
            status: 500,
            message: "boom".into(),
        }));
        let (fallback, _) = stub("b", Behaviour::Ok);
        let registry = ProviderRegistry::with_config(
            vec![provider, fallback],
            RegistryConfig {
                min_samples: 3,
                window: 5,
                open_threshold_percent: 50,
                base_cooldown: Duration::from_millis(50),
                ..Default::default()
            },
        );

        for _ in 0..6 {
            let _ = registry.fetch(request(), &ctx()).await;
        }
        let calls_after_open = calls.load(Ordering::SeqCst);
        assert!(calls_after_open >= 3);

        // While the circuit is open the provider must not be called at all.
        let outcome = registry.fetch(request(), &ctx()).await.expect("fallback");
        assert_eq!(outcome.provider.as_str(), "b");
        assert_eq!(calls.load(Ordering::SeqCst), calls_after_open);

        let health = registry.health();
        let first = health.iter().find(|h| h.provider == "a").expect("health");
        assert!(first.state == "open" || first.state == "half_open");
    }

    #[tokio::test]
    async fn provider_timeout_is_enforced_by_the_registry() {
        let (slow, _) = stub("slow", Behaviour::Delay(Duration::from_secs(5)));
        let registry = ProviderRegistry::new(vec![slow]);
        let mut short = ctx();
        short.deadline = Instant::now() + Duration::from_millis(50);
        let error = registry.fetch(request(), &short).await.expect_err("must fail");
        assert!(matches!(error, Error::Provider(ProviderError::Timeout) | Error::Timeout));
    }
}
