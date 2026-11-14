//! Transport reliability tests for the InnerTube search provider.
//!
//! What these pin, all without leaving the process:
//!
//! * the provider's **deadline** is honoured (a context that is already expired never reaches the
//!   network, and a stalled request is cancelled by its owner rather than leaking);
//! * each transport failure is mapped to the **stable provider error code** the registry and the API
//!   surface report (`provider_timeout`, `provider_transport`, `provider_parse`, `provider_upstream`);
//! * a **redirect is surfaced, not followed**, and nothing is retried behind the caller's back.
//!
//! What these cannot pin, and therefore document instead (see `docs/security/TRANSPORT_RELIABILITY.md`):
//! pooled-connection reuse across requests, the resolver guard's behaviour on a *pooled* connection,
//! and reqwest's redirect/`no_proxy` handling — all of those are `reqwest` internals that a scripted
//! transport never touches. They are covered by the resolver unit tests (per-hop address filtering)
//! and by the documented `reqwest` configuration, not by a claim this file cannot support.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, SearchFilters, SearchKind, Strictness, Transport,
    TransportError, TransportRequest, TransportResponse,
};

/// Bootstrap document shaped like `https://www.youtube.com/sw.js_data`. Placeholder values, correctly
/// shaped (same document the request-contract tests use).
const BOOTSTRAP: &str = r#"[[{"visitorData":"VISITOR-FIXTURE"}],"AIzaSYFIXTUREKEY0123456789"]"#;

/// What the scripted transport does with the *endpoint* call (call index ≥ 1). Call 0 is always the
/// bootstrap, because a provider that cannot build a session would never reach the code under test.
#[derive(Clone, Copy)]
enum Script {
    /// Answer after this delay (used to prove cancellation).
    Slow(Duration),
    /// Return the transport-level timeout the production transport raises.
    Timeout,
    /// Return the body-limit failure.
    TooLarge(usize),
    /// Return the failure a refused/reset connection produces.
    Connect(&'static str),
    /// Answer with a redirect to `Location`, which must be surfaced rather than followed.
    Redirect(&'static str),
}

struct ScriptedTransport {
    script: Script,
    calls: AtomicUsize,
}

impl ScriptedTransport {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self {
            script,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn ok(body: &str, status: u16, headers: Vec<(&str, &str)>) -> TransportResponse {
    TransportResponse {
        status,
        headers: headers
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        body: Bytes::from(body.to_owned()),
        elapsed: Duration::from_millis(2),
    }
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn execute(
        &self,
        _request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            return Ok(ok(
                BOOTSTRAP,
                200,
                vec![("content-type", "application/json")],
            ));
        }
        match self.script {
            Script::Slow(delay) => {
                tokio::time::sleep(delay).await;
                Ok(ok(r#"{"contents":[]}"#, 200, vec![]))
            }
            Script::Timeout => Err(TransportError::Timeout(Duration::from_secs(10))),
            Script::TooLarge(limit) => Err(TransportError::BodyTooLarge { limit }),
            Script::Connect(message) => Err(TransportError::Connect(message.to_owned())),
            Script::Redirect(location) => Ok(ok("", 302, vec![("location", location)])),
        }
    }
}

fn provider(transport: Arc<ScriptedTransport>) -> InnertubeProvider {
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    InnertubeProvider::new(config, transport)
}

fn context(deadline: Instant) -> ProviderContext {
    ProviderContext {
        deadline,
        request_id: "transport-reliability".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness: Strictness::Lenient,
    }
}

fn search() -> ProviderRequest {
    ProviderRequest::Search {
        query: "captain jack".into(),
        kind: SearchKind::All,
        filters: SearchFilters::default(),
        cursor: None,
        hl: "en".into(),
        gl: "US".into(),
        limit: 20,
    }
}

fn code(error: &vydra_errors::ProviderError) -> &'static str {
    error.code()
}

#[tokio::test]
async fn an_expired_deadline_never_reaches_the_network() {
    let transport = ScriptedTransport::new(Script::Slow(Duration::from_millis(1)));
    let provider = provider(transport.clone());

    let expired = Instant::now() - Duration::from_millis(1);
    let error = provider
        .fetch(search(), &context(expired))
        .await
        .expect_err("an expired context must fail");

    assert_eq!(code(&error), "provider_timeout", "error: {error}");
    assert_eq!(
        transport.calls(),
        0,
        "no request may be issued for an expired context"
    );
}

#[tokio::test]
async fn a_cancelled_future_drops_the_in_flight_request() {
    // The search call stalls for far longer than the outer timeout: cancellation must return
    // control to the caller promptly instead of waiting for the (abandoned) request.
    let transport = ScriptedTransport::new(Script::Slow(Duration::from_secs(30)));
    let provider = provider(transport.clone());

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_millis(50),
        provider.fetch(search(), &context(Instant::now() + Duration::from_secs(60))),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(outcome.is_err(), "the outer timeout must win");
    assert!(
        elapsed < Duration::from_secs(5),
        "cancellation must not wait for the request: {elapsed:?}"
    );
    assert_eq!(
        transport.calls(),
        2,
        "the cancelled call was already in flight (bootstrap + search)"
    );
}

#[tokio::test]
async fn a_transport_timeout_is_reported_as_a_provider_timeout() {
    let transport = ScriptedTransport::new(Script::Timeout);
    let provider = provider(transport);

    let error = provider
        .fetch(search(), &context(Instant::now() + Duration::from_secs(5)))
        .await
        .expect_err("timeout must surface");

    assert_eq!(code(&error), "provider_timeout", "error: {error}");
    assert!(
        error.to_string().contains("timeout"),
        "the message must say what happened: {error}"
    );
}

#[tokio::test]
async fn an_oversized_body_is_reported_as_a_parse_error_naming_the_cap() {
    let transport = ScriptedTransport::new(Script::TooLarge(4_194_304));
    let provider = provider(transport);

    let error = provider
        .fetch(search(), &context(Instant::now() + Duration::from_secs(5)))
        .await
        .expect_err("body limit must surface");

    assert_eq!(code(&error), "provider_parse", "error: {error}");
    assert!(
        error.to_string().contains("4194304"),
        "the message must name the cap: {error}"
    );
}

#[tokio::test]
async fn a_connection_failure_is_wrapped_and_not_retried() {
    let transport = ScriptedTransport::new(Script::Connect("connection reset by peer"));
    let provider = provider(transport.clone());

    let error = provider
        .fetch(search(), &context(Instant::now() + Duration::from_secs(5)))
        .await
        .expect_err("connect failure must surface");

    assert_eq!(code(&error), "provider_transport", "error: {error}");
    assert!(error.to_string().contains("connection reset"), "{error}");
    assert_eq!(
        transport.calls(),
        2,
        "the provider issues one call per fetch: retries belong to the registry, not here"
    );
}

#[tokio::test]
async fn a_redirect_is_surfaced_and_never_followed() {
    // A redirect to a host outside the allowlist is exactly what an open redirect would try. The
    // provider must report it as an upstream status; a second call would mean it was followed.
    let transport = ScriptedTransport::new(Script::Redirect(
        "https://169.254.169.254/latest/meta-data/",
    ));
    let provider = provider(transport.clone());

    let error = provider
        .fetch(search(), &context(Instant::now() + Duration::from_secs(5)))
        .await
        .expect_err("a redirect must not be treated as success");

    assert_eq!(code(&error), "provider_upstream", "error: {error}");
    assert!(
        error.to_string().contains("302"),
        "status must be reported: {error}"
    );
    assert_eq!(
        transport.calls(),
        2,
        "a followed redirect would have produced further calls"
    );
}
