//! Vydra API server binary.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpListener;
use tracing::info;
use vydra_api::{AppState, MetricsObserver, router};
use vydra_auth::{Authenticator, RateLimiter};
use vydra_cache::MemoryCache;
use vydra_common::{BuildInfo, CursorCodec};
use vydra_config::Config;
use vydra_engines::{Engines, EnginesConfig};
use vydra_observability::Metrics;
use vydra_provider_registry::{ProviderRegistry, RegistryConfig};
use vydra_providers::ReqwestTransport;
use vydra_providers::innertube::{ClientKind, InnertubeProvider, InnertubeProviderConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    vydra_observability::init_tracing(&config.observability)?;

    let build = BuildInfo::current();
    info!(
        version = build.version,
        git_sha = build.git_sha,
        config = %config.redacted_summary(),
        "starting vydra api server"
    );

    // --- providers ---
    let transport = Arc::new(ReqwestTransport::new()?);
    let mut providers: Vec<Arc<dyn vydra_providers::Provider>> = Vec::new();

    if config.providers.innertube.enabled {
        let kind = ClientKind::parse(&config.providers.innertube.default_client);
        if kind.is_none() {
            tracing::warn!(
                configured = %config.providers.innertube.default_client,
                "unknown innertube client, falling back to web"
            );
        }
        let provider_config = InnertubeProviderConfig {
            client: vydra_providers::innertube::ClientProfile::from_kind(
                kind.unwrap_or(ClientKind::Web),
                config
                    .providers
                    .innertube
                    .client_version_overrides
                    .get("web")
                    .map(String::as_str),
            ),
            hl: config.providers.default_hl.clone(),
            gl: config.providers.default_gl.clone(),
            request_timeout: Duration::from_millis(config.providers.innertube.request_timeout_ms),
            session_ttl: Duration::from_secs(config.providers.innertube.session_ttl_secs),
            max_response_bytes: config.providers.innertube.max_response_bytes,
            consent_cookie: config.providers.innertube.consent_cookie.clone(),
            api_key_override: config
                .providers
                .innertube
                .api_key_override
                .as_ref()
                .map(|secret| secret.expose().to_owned()),
            enable_unverified_filters: config.providers.enable_unverified_filters,
            ..Default::default()
        };
        providers.push(Arc::new(InnertubeProvider::new(
            Arc::new(provider_config),
            transport,
        )));
    }

    let metrics = Arc::new(Metrics::new()?);
    metrics.set_build_info(build.version);

    let observer: Arc<dyn vydra_provider_registry::ProviderObserver> =
        Arc::new(MetricsObserver::new(metrics.clone()));
    let registry = Arc::new(
        ProviderRegistry::with_config(
            providers,
            RegistryConfig {
                max_attempts: config.providers.max_attempts,
                ..Default::default()
            },
        )
        .with_observer(observer),
    );
    info!(
        providers = ?registry.provider_ids(),
        capabilities = %registry.capabilities(),
        "provider registry ready"
    );

    // --- engines ---
    let cursors = CursorCodec::new(
        config.auth.cursor_secret.expose(),
        Duration::from_secs(config.auth.cursor_ttl_secs),
    )?;
    let cache = Arc::new(MemoryCache::new(config.cache.max_entries));
    let engines = Arc::new(Engines::new(
        registry.clone(),
        cache,
        cursors,
        EnginesConfig {
            hl: config.providers.default_hl.clone(),
            gl: config.providers.default_gl.clone(),
            call_budget: Duration::from_millis(config.server.request_timeout_ms),
            metadata_ttl: Duration::from_secs(config.cache.metadata_ttl_secs),
            search_ttl: Duration::from_secs(config.cache.search_ttl_secs),
            cache_search: config.cache.cache_search,
            strictness: vydra_providers::Strictness::Lenient,
            unverified_filters: config.providers.enable_unverified_filters,
        },
    ));

    // --- edge services ---
    let authenticator = Arc::new(Authenticator::from_config(&config.auth)?);
    if config.auth.bootstrap_key.is_some() {
        tracing::warn!("auth.bootstrap_key is configured: development only, refused in production");
    }
    let limiter = Arc::new(RateLimiter::new(Duration::from_secs(60), 100_000));

    let state = Arc::new(AppState {
        engines,
        registry,
        authenticator,
        limiter,
        metrics,
        config: Arc::new(config.clone()),
        build,
        started: Instant::now(),
    });

    let app = router(state);
    let listener = TcpListener::bind((config.server.host.as_str(), config.server.port)).await?;
    info!(
        address = %listener.local_addr()?,
        "listening"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(Duration::from_millis(
        config.server.shutdown_grace_ms,
    )))
    .await?;
    Ok(())
}

async fn shutdown_signal(grace: Duration) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!(grace_ms = grace.as_millis() as u64, "shutdown requested");
}
