//! HTTP edge: routing, handlers, envelope and error mapping.
//!
//! Only this crate knows about HTTP. It never sees provider JSON.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Instrument;
use uuid::Uuid;
use vydra_common::{BuildInfo, PageParams, PaginationMeta, VideoId};
use vydra_config::Config;
use vydra_domain::{Provenance, SearchResults, SourceAttempt, VideoDocument};
use vydra_engines::{Engines, SearchInput, VideoInput};
use vydra_errors::{Error, ErrorBody};
use vydra_observability::Metrics;
use vydra_provider_registry::ProviderRegistry;
use vydra_providers::{Capability, SearchKind};
use vydra_auth::{Authenticator, RateLimiter};

/// Request identifier attached by middleware.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared application state.
#[derive(Debug)]
pub struct AppState {
    pub engines: Arc<Engines>,
    pub registry: Arc<ProviderRegistry>,
    pub authenticator: Arc<Authenticator>,
    pub limiter: Arc<RateLimiter>,
    pub metrics: Arc<Metrics>,
    pub config: Arc<Config>,
    pub build: BuildInfo,
    pub started: Instant,
}

/// Response metadata.
#[derive(Debug, Serialize)]
pub struct Meta {
    pub request_id: String,
    pub provider: Option<String>,
    pub sources: Vec<SourceAttempt>,
    pub cached: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    pub warnings: Vec<String>,
    pub parse_warnings: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<PaginationMeta>,
}

impl Meta {
    fn from_provenance(request_id: &str, provenance: &Provenance, pagination: Option<PaginationMeta>) -> Self {
        Self {
            request_id: request_id.to_owned(),
            provider: Some(provenance.provider.clone()),
            sources: Vec::new(),
            cached: provenance.cached,
            fetched_at: provenance.fetched_at,
            warnings: Vec::new(),
            parse_warnings: provenance.parse_warnings,
            pagination,
        }
    }
}

/// Standard success envelope.
#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
}

/// Handler result type: failures are converted into the public error contract at the edge.
pub type ApiResult<T> = std::result::Result<T, ApiError>;

/// Standard error response.
#[derive(Debug)]
pub struct ApiError {
    error: Error,
    request_id: String,
}

impl ApiError {
    pub fn new(error: Error, request_id: &str) -> Self {
        Self {
            error,
            request_id: request_id.to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody::from_error(&self.error, self.request_id.clone());
        let status = StatusCode::from_u16(body.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = (status, Json(serde_json::json!({ "error": body }))).into_response();
        if let Some(retry_after) = body.retry_after_ms {
            let seconds = (retry_after / 1000).max(1).to_string();
            if let Ok(value) = HeaderValue::from_str(&seconds) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

// --- query payloads -------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub limit: Option<u16>,
    pub cursor: Option<String>,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl SearchQuery {
    fn into_input(self, kind: SearchKind) -> vydra_errors::Result<SearchInput> {
        let query = self.q.trim().to_owned();
        if query.is_empty() {
            return Err(Error::validation("q", "required"));
        }
        if query.chars().count() > 200 {
            return Err(Error::validation("q", "too_long"));
        }
        if query.chars().any(char::is_control) {
            return Err(Error::validation("q", "invalid_characters"));
        }
        Ok(SearchInput {
            query,
            kind,
            filters: Default::default(),
            page: PageParams {
                limit: self.limit,
                cursor: self.cursor,
            },
            hl: self.hl,
            gl: self.gl,
        })
    }
}

// --- handlers -------------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn ready() -> impl IntoResponse {
    // Readiness reflects only in-process dependencies; Postgres is checked when configured, which
    // happens in `AppState` wiring once the repository layer lands (Phase 8).
    Json(serde_json::json!({"status": "ready"}))
}

#[derive(Serialize)]
struct StatusResponse {
    build: BuildInfoResponse,
    uptime_seconds: u64,
    environment: String,
    capabilities: Vec<String>,
    providers: Vec<vydra_provider_registry::ProviderHealthSnapshot>,
}

#[derive(Serialize)]
struct BuildInfoResponse {
    version: &'static str,
    git_sha: &'static str,
}

async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let capabilities = state
        .registry
        .capabilities()
        .iter()
        .map(|cap| cap.as_str().to_owned())
        .collect();
    Json(StatusResponse {
        build: BuildInfoResponse {
            version: state.build.version,
            git_sha: state.build.git_sha,
        },
        uptime_seconds: state.started.elapsed().as_secs(),
        environment: state.config.environment.as_str().to_owned(),
        capabilities,
        providers: state.registry.health(),
    })
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.metrics.encode() {
        Ok(text) => (StatusCode::OK, text).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "").into_response(),
    }
}

async fn search_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Envelope<SearchResults>>> {
    search_impl(state, request_id, query, SearchKind::All).await
}

async fn search_videos_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Envelope<SearchResults>>> {
    search_impl(state, request_id, query, SearchKind::Videos).await
}

async fn search_channels_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Envelope<SearchResults>>> {
    search_impl(state, request_id, query, SearchKind::Channels).await
}

async fn search_playlists_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Envelope<SearchResults>>> {
    search_impl(state, request_id, query, SearchKind::Playlists).await
}

async fn search_impl(
    state: Arc<AppState>,
    request_id: RequestId,
    query: SearchQuery,
    kind: SearchKind,
) -> ApiResult<Json<Envelope<SearchResults>>> {
    let limit = query.limit;
    let input = query.into_input(kind).map_err(|e| ApiError::new(e, &request_id.0))?;
    let results = state
        .engines
        .search(input)
        .instrument(tracing::info_span!("engine.search", kind = kind.cursor_namespace()))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        results.items.len(),
        results.next_cursor.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &results.provenance, Some(pagination));
    Ok(Json(Envelope { data: results, meta }))
}

async fn video_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> ApiResult<Json<Envelope<VideoDocument>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let document = state
        .engines
        .video(VideoInput {
            id: video_id,
            hl: None,
            gl: None,
        })
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &document.provenance, None);
    Ok(Json(Envelope { data: document, meta }))
}

async fn chapters_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> ApiResult<Json<Envelope<vydra_domain::VideoChapters>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let chapters = state
        .engines
        .chapters(VideoInput {
            id: video_id,
            hl: None,
            gl: None,
        })
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &chapters.provenance, None);
    Ok(Json(Envelope { data: chapters, meta }))
}

async fn captions_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> ApiResult<Json<Envelope<vydra_domain::CaptionTrackList>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let tracks = state
        .engines
        .captions(VideoInput {
            id: video_id,
            hl: None,
            gl: None,
        })
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &tracks.provenance, None);
    Ok(Json(Envelope { data: tracks, meta }))
}

// --- middleware -----------------------------------------------------------------------------

async fn request_id_middleware(mut request: Request<axum::body::Body>, next: Next) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .map(|value| RequestId(value.to_owned()))
        .unwrap_or_default();
    request.extensions_mut().insert(id.clone());
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&id.0) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();

    let api_key = request
        .headers()
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::to_owned)
        });

    let principal = match state.authenticator.authenticate(api_key.as_deref()) {
        Ok(principal) => principal,
        Err(error) => return ApiError::new(error, &request_id).into_response(),
    };

    let bucket = if principal.anonymous {
        "anonymous".to_owned()
    } else {
        format!("key:{}", principal.key_id)
    };
    if let Err(error) = state.limiter.check(&bucket, state.config.auth.per_key_rpm) {
        return ApiError::new(error, &request_id).into_response();
    }

    request.extensions_mut().insert(principal);
    next.run(request).await
}

// --- router ---------------------------------------------------------------------------------

/// Build the router. Endpoints whose capability is not declared by any provider are **not mounted**
/// (`docs/architecture/ARCHITECTURE.md` §5).
pub fn router(state: Arc<AppState>) -> Router {
    let capabilities = state.registry.capabilities();
    let mut api = Router::new().route("/search", get(search_handler));

    if capabilities.contains(Capability::SearchVideos) {
        api = api.route("/search/videos", get(search_videos_handler));
    }
    if capabilities.contains(Capability::SearchChannels) {
        api = api.route("/search/channels", get(search_channels_handler));
    }
    if capabilities.contains(Capability::SearchPlaylists) {
        api = api.route("/search/playlists", get(search_playlists_handler));
    }
    if capabilities.contains(Capability::VideoMetadata) {
        api = api
            .route("/videos/{id}", get(video_handler))
            .route("/videos/{id}/metadata", get(video_handler));
    }
    if capabilities.contains(Capability::VideoChapters) {
        api = api.route("/videos/{id}/chapters", get(chapters_handler));
    }
    if capabilities.contains(Capability::CaptionsMetadata) {
        api = api.route("/videos/{id}/captions", get(captions_handler));
    }

    let mut router = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/v1/health", get(health))
        .route("/api/v1/ready", get(ready))
        .route("/api/v1/status", get(status))
        .route("/api/v1/metrics", get(metrics))
        .route("/status", get(status))
        .route("/metrics", get(metrics))
        .nest("/api/v1", api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::new(Duration::from_millis(
            state.config.server.request_timeout_ms,
        )))
        .layer(RequestBodyLimitLayer::new(state.config.server.body_limit_bytes))
        .with_state(state);

    router
}

#[cfg(test)]
mod tests {
    use super::*;
    use vydra_auth::Principal;

    #[test]
    fn request_ids_are_unique_and_ulid_like() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a.0, b.0);
        assert_eq!(a.0.len(), 36);
    }

    #[test]
    fn search_query_validation() {
        let query = SearchQuery {
            q: "  ".into(),
            limit: None,
            cursor: None,
            hl: None,
            gl: None,
        };
        assert!(matches!(
            query.into_input(SearchKind::All),
            Err(Error::Validation(_))
        ));
    }

    #[test]
    fn oversized_query_is_rejected() {
        let query = SearchQuery {
            q: "a".repeat(201),
            limit: None,
            cursor: None,
            hl: None,
            gl: None,
        };
        assert!(query.into_input(SearchKind::All).is_err());
    }

    #[test]
    fn principals_are_not_exposed_in_meta() {
        let provenance = Provenance {
            provider: "innertube-web".into(),
            endpoint: "search".into(),
            client: Some("web-1".into()),
            fetched_at: OffsetDateTime::now_utc(),
            cached: false,
            parse_warnings: 0,
        };
        let meta = Meta::from_provenance("rid", &provenance, None);
        assert_eq!(meta.request_id, "rid");
        assert_eq!(meta.provider.as_deref(), Some("innertube-web"));
        let principal = Principal::anonymous();
        let json = serde_json::to_string(&meta).expect("serialize");
        assert!(!json.contains(&principal.key_id) || principal.key_id == "anonymous");
    }
}
