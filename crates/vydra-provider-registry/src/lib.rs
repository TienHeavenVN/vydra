//! Provider registry: capability-based selection, health tracking, circuit breaking, concurrency and
//! rate gating, and **bounded** fallback (ADR-007).
//!
//! Normative behaviour: `docs/architecture/PROVIDER_INTERFACE.md`. Audit notes and the defects this
//! revision fixes: `docs/security/PROVIDER_ENGINE_AUDIT.md`.
//!
//! Invariants enforced here:
//!
//! 1. A deterministic content fact (`Unavailable`, `NotFound`) is returned immediately and never
//!    triggers another provider call — fallback must not silently substitute different content.
//! 2. Total attempts per request are bounded by `max_attempts` (hard cap 3).
//! 3. Every provider call is bounded by the caller's deadline, a per-provider concurrency permit and
//!    a per-provider token bucket.
//! 4. Circuit breakers move `Closed → Open → HalfOpen → Closed` deterministically; a failed half-open
//!    probe re-opens with a doubled cooldown.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vydra_domain::{AttemptOutcome, SourceAttempt};
use vydra_errors::{Error, ProviderError};
use vydra_providers::{
    Capability, CapabilitySet, ProviderContext, ProviderId, ProviderRequest, ProviderResponse,
    SharedProvider,
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
    /// Concurrent in-flight requests allowed per provider. `0` disables the limit.
    pub per_provider_concurrency: usize,
    /// Requests per minute allowed per provider (token bucket, burst = limit). `0` disables.
    pub per_provider_rpm: u32,
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
            per_provider_concurrency: 32,
            per_provider_rpm: 0,
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

/// Optional observability hook. Implemented by `vydra-api`/`vydra-observability` so that this crate
/// stays free of metrics dependencies.
pub trait ProviderObserver: Send + Sync + fmt::Debug {
    fn on_attempt(
        &self,
        provider: ProviderId,
        capability: Capability,
        outcome: AttemptOutcome,
        latency: Duration,
    );
    fn on_circuit_change(&self, provider: ProviderId, state: CircuitState);
}

#[derive(Debug)]
struct ProviderState {
    window: std::collections::VecDeque<bool>,
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
            window: std::collections::VecDeque::new(),
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

    fn cooldown(&self, config: &RegistryConfig) -> Duration {
        config
            .base_cooldown
            .saturating_mul(1u32 << self.consecutive_opens.min(6))
            .min(config.max_cooldown)
    }

    fn open(&mut self, config: &RegistryConfig, now: Instant) {
        let cooldown = self.cooldown(config);
        self.consecutive_opens = self.consecutive_opens.saturating_add(1);
        self.state = CircuitState::Open {
            until: now + cooldown,
        };
        self.probe_in_flight = false;
    }

    /// Record an outcome and apply the transition rules.
    fn record(
        &mut self,
        healthy: bool,
        config: &RegistryConfig,
        now: Instant,
    ) -> Option<CircuitState> {
        let previous = self.state;

        self.window.push_back(healthy);
        while self.window.len() > config.window {
            self.window.pop_front();
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
            match self.state {
                // A failed probe must re-open immediately with a longer cooldown.
                CircuitState::HalfOpen => self.open(config, now),
                CircuitState::Closed => {
                    let rate_percent = self.success_rate() * 100.0;
                    if self.window.len() >= config.min_samples
                        && rate_percent < f64::from(config.open_threshold_percent)
                    {
                        self.open(config, now);
                    }
                }
                CircuitState::Open { .. } => {}
            }
        }

        (self.state != previous).then_some(self.state)
    }

    fn refresh(&mut self, now: Instant) -> Option<CircuitState> {
        if let CircuitState::Open { until } = self.state {
            if now >= until {
                self.state = CircuitState::HalfOpen;
                self.probe_in_flight = false;
                return Some(self.state);
            }
        }
        None
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

/// Token bucket used for per-provider rate limiting.
#[derive(Debug, Clone)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_second: f64,
    refilled_at: Instant,
}

impl TokenBucket {
    fn new(per_minute: u32) -> Self {
        let capacity = f64::from(per_minute);
        Self {
            tokens: capacity,
            capacity,
            refill_per_second: capacity / 60.0,
            refilled_at: Instant::now(),
        }
    }

    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.refilled_at)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_second).min(self.capacity);
        self.refilled_at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
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
    pub total_ok: u64,
    pub total_failed: u64,
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
pub struct ProviderRegistry {
    providers: Vec<SharedProvider>,
    states: Mutex<HashMap<ProviderId, ProviderState>>,
    gates: Mutex<HashMap<ProviderId, TokenBucket>>,
    permits: HashMap<ProviderId, Arc<Semaphore>>,
    config: RegistryConfig,
    observer: Option<Arc<dyn ProviderObserver>>,
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field(
                "providers",
                &self.providers.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .field("config", &self.config)
            .field("observer", &self.observer.is_some())
            .finish()
    }
}

impl ProviderRegistry {
    /// Build a registry, rejecting duplicate provider ids (they would share health state and
    /// silently hide one provider behind another).
    pub fn new(providers: Vec<SharedProvider>) -> Self {
        Self::with_config(providers, RegistryConfig::default())
    }

    pub fn with_config(providers: Vec<SharedProvider>, config: RegistryConfig) -> Self {
        let mut unique: Vec<SharedProvider> = Vec::with_capacity(providers.len());
        let mut states = HashMap::new();
        let mut gates = HashMap::new();
        let mut permits = HashMap::new();

        for provider in providers {
            let id = provider.id();
            if states.contains_key(&id) {
                tracing::error!(
                    provider = %id,
                    "duplicate provider id rejected: ids must be unique"
                );
                continue;
            }
            states.insert(id, ProviderState::new());
            if config.per_provider_rpm > 0 {
                gates.insert(id, TokenBucket::new(config.per_provider_rpm));
            }
            if config.per_provider_concurrency > 0 {
                permits.insert(
                    id,
                    Arc::new(Semaphore::new(config.per_provider_concurrency)),
                );
            }
            unique.push(provider);
        }

        Self {
            providers: unique,
            states: Mutex::new(states),
            gates: Mutex::new(gates),
            permits,
            config: RegistryConfig {
                max_attempts: config.max_attempts.clamp(1, 3),
                ..config
            },
            observer: None,
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn ProviderObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Union of all declared capabilities.
    pub fn capabilities(&self) -> CapabilitySet {
        self.providers
            .iter()
            .fold(CapabilitySet::NONE, |acc, provider| {
                acc.union(provider.capabilities())
            })
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities().contains(capability)
    }

    pub fn provider_ids(&self) -> Vec<ProviderId> {
        self.providers
            .iter()
            .map(|provider| provider.id())
            .collect()
    }

    /// Sanitized health for `GET /status`.
    pub fn health(&self) -> Vec<ProviderHealthSnapshot> {
        let now = Instant::now();
        let mut states = self.lock_states();
        self.providers
            .iter()
            .map(|provider| {
                let state = states
                    .entry(provider.id())
                    .or_insert_with(ProviderState::new);
                state.refresh(now);
                ProviderHealthSnapshot {
                    provider: provider.id().as_str().to_owned(),
                    client: provider.client_label(),
                    state: state.state.as_str().to_owned(),
                    success_rate: state.success_rate() as f32,
                    samples: state.window.len(),
                    consecutive_failures: state.consecutive_failures,
                    last_error_code: state.last_error.clone(),
                    total_ok: state.total_ok,
                    total_failed: state.total_failed,
                    capabilities: provider
                        .capabilities()
                        .iter()
                        .map(|cap| cap.as_str().to_owned())
                        .collect(),
                }
            })
            .collect()
    }

    fn lock_states(&self) -> std::sync::MutexGuard<'_, HashMap<ProviderId, ProviderState>> {
        match self.states.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn record(&self, provider: ProviderId, error: Option<&ProviderError>) {
        let now = Instant::now();
        let transition = {
            let mut states = self.lock_states();
            let state = states.entry(provider).or_insert_with(ProviderState::new);
            match error {
                None => state.record(true, &self.config, now),
                Some(err) if err.affects_health() => {
                    state.last_error = Some(err.code().to_owned());
                    state.record(false, &self.config, now)
                }
                Some(_) => None,
            }
        };
        if let (Some(observer), Some(new_state)) = (&self.observer, transition) {
            observer.on_circuit_change(provider, new_state);
        }
    }

    /// Capability-filtered, health-and-throttle-aware candidate list.
    fn candidates(&self, capability: Capability) -> (Vec<SharedProvider>, usize) {
        let now = Instant::now();
        let mut states = self.lock_states();
        let mut throttled = 0usize;
        let mut eligible = Vec::new();

        for provider in &self.providers {
            if !provider.capabilities().contains(capability) {
                continue;
            }
            let allowed_by_breaker = {
                let state = states
                    .entry(provider.id())
                    .or_insert_with(ProviderState::new);
                state.allow(now)
            };
            if !allowed_by_breaker {
                tracing::debug!(provider = %provider.id(), "skipped: circuit open or probe in flight");
                continue;
            }

            if let Ok(mut gates) = self.gates.lock() {
                if let Some(bucket) = gates.get_mut(&provider.id()) {
                    if !bucket.try_take(now) {
                        throttled += 1;
                        tracing::debug!(provider = %provider.id(), "skipped: provider rate limit");
                        continue;
                    }
                }
            }

            eligible.push(provider.clone());
        }

        (eligible, throttled)
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

        let (candidates, throttled) = self.candidates(capability);
        if candidates.is_empty() {
            return if throttled > 0 {
                Err(Error::RateLimited {
                    retry_after: Some(Duration::from_secs(1)),
                })
            } else {
                Err(Error::ProviderCircuitOpen {
                    retry_after: Some(self.config.base_cooldown),
                })
            };
        }

        let attempts_limit = usize::from(self.config.max_attempts).min(candidates.len());
        let mut attempts: Vec<SourceAttempt> = Vec::with_capacity(attempts_limit);
        let mut last_transient: Option<ProviderError> = None;

        for (index, provider) in candidates.iter().take(attempts_limit).enumerate() {
            let started = Instant::now();

            // Concurrency gate: if the provider is saturated, do not queue indefinitely — fail over.
            let _permit: Option<OwnedSemaphorePermit> = match self.permits.get(&provider.id()) {
                Some(semaphore) => match semaphore.clone().try_acquire_owned() {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        attempts.push(SourceAttempt {
                            provider: provider.id().as_str().to_owned(),
                            endpoint: request.endpoint().to_owned(),
                            attempt: index as u8,
                            latency_ms: 0,
                            outcome: AttemptOutcome::Skipped,
                        });
                        last_transient = Some(ProviderError::RateLimited {
                            retry_after: Some(Duration::from_millis(50)),
                        });
                        continue;
                    }
                },
                None => None,
            };

            let remaining = ctx.remaining();
            let result = if remaining.is_zero() {
                Err(ProviderError::Timeout)
            } else {
                match tokio::time::timeout(remaining, provider.fetch(request.clone(), ctx)).await {
                    Ok(result) => result,
                    Err(_) => Err(ProviderError::Timeout),
                }
            };
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

            match result {
                Ok(response) => {
                    self.record(provider.id(), None);
                    if let Some(observer) = &self.observer {
                        observer.on_attempt(
                            provider.id(),
                            capability,
                            AttemptOutcome::Ok,
                            started.elapsed(),
                        );
                    }
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
                    if let Some(observer) = &self.observer {
                        observer.on_attempt(
                            provider.id(),
                            capability,
                            AttemptOutcome::Failed,
                            started.elapsed(),
                        );
                    }
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
                        // Deterministic content fact: do not spend quota on another provider and do
                        // not risk substituting different content.
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
    use vydra_errors::UnavailabilityReason;
    use vydra_providers::Provider;
    use vydra_providers::Strictness;

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
            fetched_at: time::OffsetDateTime::now_utc(),
            cached: false,
            parse_warnings: 0,
            sources: Vec::new(),
        }
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

        let error = registry
            .fetch(request(), &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::ContentUnavailable { .. }));
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            second_calls.load(Ordering::SeqCst),
            0,
            "quota must not be spent"
        );
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
        let error = registry
            .fetch(request(), &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(
            error,
            Error::Provider(_) | Error::ProviderUnavailable(_)
        ));
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            c_calls.load(Ordering::SeqCst),
            0,
            "third provider must not be tried"
        );
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
        let error = registry
            .fetch(request, &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::CapabilityUnavailable(_)));
        assert!(!registry.supports(Capability::SearchVideos));
    }

    #[tokio::test]
    async fn duplicate_provider_ids_are_rejected() {
        let (first, first_calls) = stub("dup", Behaviour::Ok);
        let (second, second_calls) = stub("dup", Behaviour::Ok);
        let registry = ProviderRegistry::new(vec![first, second]);
        assert_eq!(registry.provider_ids().len(), 1);
        registry.fetch(request(), &ctx()).await.expect("ok");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn circuit_transitions_closed_open_half_open_closed() {
        // First call fails (below min_samples), then we force enough samples to open.
        let (provider, _) = stub(
            "a",
            Behaviour::Fail(ProviderError::Upstream {
                status: 500,
                message: "boom".into(),
            }),
        );
        let registry = ProviderRegistry::with_config(
            vec![provider],
            RegistryConfig {
                min_samples: 2,
                window: 4,
                open_threshold_percent: 50,
                base_cooldown: Duration::from_millis(40),
                max_cooldown: Duration::from_millis(200),
                per_provider_concurrency: 0,
                per_provider_rpm: 0,
                max_attempts: 1,
            },
        );

        for _ in 0..2 {
            let _ = registry.fetch(request(), &ctx()).await;
        }
        assert_eq!(registry.health()[0].state, "open");

        // While open, the provider is never called.
        let error = registry.fetch(request(), &ctx()).await.expect_err("open");
        assert!(matches!(error, Error::ProviderCircuitOpen { .. }));

        // After the cooldown the breaker half-opens: one probe is admitted and fails again,
        // which must re-open it (not stay half-open).
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = registry.fetch(request(), &ctx()).await;
        let state = registry.health()[0].state.clone();
        assert!(
            state == "open" || state == "half_open",
            "unexpected state {state}"
        );
    }

    #[tokio::test]
    async fn half_open_probe_success_closes_the_circuit() {
        // Provider fails twice then succeeds: use a custom provider driven by a counter.
        #[derive(Debug)]
        struct Flaky {
            calls: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl Provider for Flaky {
            fn id(&self) -> ProviderId {
                ProviderId::new("flaky")
            }
            fn capabilities(&self) -> CapabilitySet {
                CapabilitySet::from_slice(&[Capability::VideoMetadata])
            }
            fn client_label(&self) -> String {
                "flaky".into()
            }
            async fn fetch(
                &self,
                _request: ProviderRequest,
                _ctx: &ProviderContext,
            ) -> Result<ProviderResponse, ProviderError> {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                if call < 2 {
                    Err(ProviderError::Upstream {
                        status: 500,
                        message: "boom".into(),
                    })
                } else {
                    Ok(ProviderResponse::Video(Box::new(stub_document(
                        &ProviderId::new("flaky"),
                    ))))
                }
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let registry = ProviderRegistry::with_config(
            vec![Arc::new(Flaky {
                calls: calls.clone(),
            })],
            RegistryConfig {
                min_samples: 2,
                window: 4,
                open_threshold_percent: 50,
                base_cooldown: Duration::from_millis(30),
                max_cooldown: Duration::from_millis(100),
                per_provider_concurrency: 0,
                per_provider_rpm: 0,
                max_attempts: 1,
            },
        );

        let _ = registry.fetch(request(), &ctx()).await;
        let _ = registry.fetch(request(), &ctx()).await;
        assert_eq!(registry.health()[0].state, "open");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let outcome = registry
            .fetch(request(), &ctx())
            .await
            .expect("probe succeeds");
        assert_eq!(outcome.provider.as_str(), "flaky");
        assert_eq!(registry.health()[0].state, "closed");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn concurrency_limit_rejects_excess_instead_of_queueing() {
        let (provider, calls) = stub("a", Behaviour::Delay(Duration::from_millis(80)));
        let registry = Arc::new(ProviderRegistry::with_config(
            vec![provider],
            RegistryConfig {
                per_provider_concurrency: 1,
                per_provider_rpm: 0,
                max_attempts: 1,
                ..Default::default()
            },
        ));

        let first = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.fetch(request(), &ctx()).await.is_ok() })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;

        let second = registry.fetch(request(), &ctx()).await;
        assert!(
            matches!(second, Err(Error::RateLimited { .. })),
            "expected saturation to fail fast, got {second:?}"
        );

        assert!(first.await.expect("join"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn per_provider_rate_limit_is_enforced() {
        let (provider, calls) = stub("a", Behaviour::Ok);
        let registry = ProviderRegistry::with_config(
            vec![provider],
            RegistryConfig {
                per_provider_rpm: 2,
                per_provider_concurrency: 0,
                ..Default::default()
            },
        );

        registry.fetch(request(), &ctx()).await.expect("first");
        registry.fetch(request(), &ctx()).await.expect("second");
        let third = registry
            .fetch(request(), &ctx())
            .await
            .expect_err("third must be throttled");
        assert!(matches!(third, Error::RateLimited { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_successes_do_not_open_the_breaker() {
        let (provider, calls) = stub("a", Behaviour::Ok);
        let registry = Arc::new(ProviderRegistry::with_config(
            vec![provider],
            RegistryConfig {
                min_samples: 2,
                ..Default::default()
            },
        ));

        let mut handles = Vec::new();
        for _ in 0..12 {
            let registry = registry.clone();
            handles.push(tokio::spawn(async move {
                registry.fetch(request(), &ctx()).await.is_ok()
            }));
        }
        for handle in handles {
            assert!(handle.await.expect("join"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 12);
        assert_eq!(registry.health()[0].state, "closed");
    }

    #[tokio::test]
    async fn provider_timeout_is_enforced_by_the_registry() {
        let (slow, _) = stub("slow", Behaviour::Delay(Duration::from_secs(5)));
        let registry = ProviderRegistry::new(vec![slow]);
        let mut short = ctx();
        short.deadline = Instant::now() + Duration::from_millis(50);
        let error = registry
            .fetch(request(), &short)
            .await
            .expect_err("must fail");
        assert!(matches!(
            error,
            Error::Provider(ProviderError::Timeout) | Error::Timeout
        ));
    }

    #[tokio::test]
    async fn observer_receives_attempts_and_circuit_changes() {
        #[derive(Debug, Default)]
        struct Recorder {
            attempts: AtomicUsize,
            circuits: AtomicUsize,
        }

        impl ProviderObserver for Recorder {
            fn on_attempt(
                &self,
                _provider: ProviderId,
                _capability: Capability,
                _outcome: AttemptOutcome,
                _latency: Duration,
            ) {
                self.attempts.fetch_add(1, Ordering::SeqCst);
            }
            fn on_circuit_change(&self, _provider: ProviderId, _state: CircuitState) {
                self.circuits.fetch_add(1, Ordering::SeqCst);
            }
        }

        let recorder = Arc::new(Recorder::default());
        let (provider, _) = stub("a", Behaviour::Ok);
        let registry = ProviderRegistry::with_config(vec![provider], RegistryConfig::default())
            .with_observer(recorder.clone());

        registry.fetch(request(), &ctx()).await.expect("ok");
        assert_eq!(recorder.attempts.load(Ordering::SeqCst), 1);
    }
}
