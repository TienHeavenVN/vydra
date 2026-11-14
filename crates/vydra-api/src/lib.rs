//! HTTP edge: routing, handlers, envelope and error mapping.
//!
//! Only this crate knows about HTTP. It never sees provider JSON.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path, Query, State};
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
use vydra_auth::{Authenticator, RateLimiter};
use vydra_common::{
    BuildInfo, ChannelRef, CommentId, PageParams, PaginationMeta, PlaylistId, VideoId,
};
use vydra_config::Config;
use vydra_domain::{
    AttemptOutcome, ChannelDocument, ChannelVideoList, CommentList, CommentSort, PlaylistDocument,
    PlaylistVideoList, Provenance, ReplyList, SearchResults, SourceAttempt, VideoDocument,
};
use vydra_engines::{
    ChannelInput, ChannelVideosInput, CommentRepliesInput, CommentsInput, Engines, PlaylistInput,
    PlaylistVideosInput, SearchInput, VideoInput,
};
use vydra_errors::{Error, ErrorBody};
use vydra_observability::Metrics;
use vydra_provider_registry::{CircuitState, ProviderObserver, ProviderRegistry};
use vydra_providers::{
    Capability, ProviderId, SearchDate, SearchDuration, SearchFeature, SearchFilters, SearchKind,
    SearchSort,
};

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
    fn from_provenance(
        request_id: &str,
        provenance: &Provenance,
        pagination: Option<PaginationMeta>,
    ) -> Self {
        Self {
            request_id: request_id.to_owned(),
            provider: Some(provenance.provider.clone()),
            sources: provenance.sources.clone(),
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
    pub sort: Option<String>,
    pub since: Option<String>,
    pub duration: Option<String>,
    /// Comma-separated feature list (`live,hd,subtitles`).
    pub filter: Option<String>,
}

impl SearchQuery {
    /// Translate the HTTP query into a typed engine input.
    ///
    /// Audit finding A-10: `sort/since/duration/filter` were accepted by the OpenAPI document but
    /// silently dropped. They are now parsed strictly, rejected when the deployment has not opted
    /// into unverified filters, and rejected with `capability_unavailable` instead of being ignored.
    fn into_input(
        self,
        kind: SearchKind,
        filters_allowed: bool,
    ) -> vydra_errors::Result<SearchInput> {
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

        let hl = normalise_locale("hl", self.hl)?;
        let gl = normalise_locale("gl", self.gl)?;

        let filters = SearchFilters {
            sort: match self.sort.as_deref() {
                None => SearchSort::Relevance,
                Some("relevance") => SearchSort::Relevance,
                Some("upload_date") => SearchSort::UploadDate,
                Some("view_count") => SearchSort::ViewCount,
                Some("rating") => SearchSort::Rating,
                Some(_) => return Err(Error::validation("sort", "unsupported")),
            },
            uploaded: match self.since.as_deref() {
                None => None,
                Some("hour") => Some(SearchDate::Hour),
                Some("day") => Some(SearchDate::Day),
                Some("week") => Some(SearchDate::Week),
                Some("month") => Some(SearchDate::Month),
                Some("year") => Some(SearchDate::Year),
                Some(_) => return Err(Error::validation("since", "unsupported")),
            },
            duration: match self.duration.as_deref() {
                None => None,
                Some("short") => Some(SearchDuration::Short),
                Some("medium") => Some(SearchDuration::Medium),
                Some("long") => Some(SearchDuration::Long),
                Some(_) => return Err(Error::validation("duration", "unsupported")),
            },
            features: parse_features(self.filter.as_deref())?,
        };

        let non_default = filters.sort != SearchSort::Relevance
            || filters.uploaded.is_some()
            || filters.duration.is_some()
            || !filters.features.is_empty();
        if non_default && !filters_allowed {
            return Err(Error::CapabilityUnavailable("search_filters".into()));
        }

        Ok(SearchInput {
            query,
            kind,
            filters,
            page: PageParams {
                limit: self.limit,
                cursor: self.cursor,
            },
            hl,
            gl,
        })
    }
}

/// Query parameters accepted by the video endpoints (Phase 5B).
///
/// `/videos/{id}` had no locale support at all: the handler hard-coded `hl`/`gl` to `None` even though
/// the engine, the cache key and the provider all take them. A localized title was therefore
/// unreachable through the API, which is the opposite of what `API_CONTRACTS.md` §5 promised.
#[derive(Debug, Default, Deserialize)]
pub struct VideoQuery {
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl VideoQuery {
    fn into_input(self, id: VideoId) -> vydra_errors::Result<VideoInput> {
        Ok(VideoInput {
            id,
            hl: normalise_locale("hl", self.hl)?,
            gl: normalise_locale("gl", self.gl)?,
        })
    }
}

/// Query parameters accepted by `GET /channels/{id}` and `GET /playlists/{id}`.
///
/// A resource endpoint maps only the context dimensions it uses; both are validated by the same
/// [`normalise_locale`] as search and the video endpoints so that a locale can never reach a provider
/// string (or a cache key) unchecked.
#[derive(Debug, Default, Deserialize)]
pub struct ResourceQuery {
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl ResourceQuery {
    fn locales(&self) -> vydra_errors::Result<(Option<String>, Option<String>)> {
        Ok((
            normalise_locale("hl", self.hl.clone())?,
            normalise_locale("gl", self.gl.clone())?,
        ))
    }
}

/// Query parameters accepted by `GET /videos/{id}/comments`.
///
/// `sort` is validated against the two orders whose request contracts are verified
/// (`docs/research/COMMENTS_REQUEST_CONTRACT.md` §3). An unknown order is a `400`, not a silent fallback to
/// the default: a client that asked for "newest" and received "top" would have no way to tell.
#[derive(Debug, Default, Deserialize)]
pub struct CommentsQuery {
    pub sort: Option<String>,
    pub limit: Option<u16>,
    pub cursor: Option<String>,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl CommentsQuery {
    fn sort(&self) -> vydra_errors::Result<CommentSort> {
        match self.sort.as_deref() {
            None | Some("top") => Ok(CommentSort::Top),
            Some("newest") => Ok(CommentSort::Newest),
            Some(_) => Err(vydra_errors::Error::validation("sort", "unsupported")),
        }
    }

    fn page(&self) -> PageParams {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }

    fn locales(&self) -> vydra_errors::Result<(Option<String>, Option<String>)> {
        Ok((
            normalise_locale("hl", self.hl.clone())?,
            normalise_locale("gl", self.gl.clone())?,
        ))
    }
}

/// Query parameters accepted by `GET /videos/{id}/comments/{comment_id}/replies`.
///
/// One struct rather than two extractors, because a handler can only hold one `Query` and splitting the
/// parameters across two types would make the second set silently unreachable.
#[derive(Debug, Default, Deserialize)]
pub struct RepliesQuery {
    pub limit: Option<u16>,
    pub cursor: Option<String>,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl RepliesQuery {
    fn page(&self) -> PageParams {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }

    fn locales(&self) -> vydra_errors::Result<(Option<String>, Option<String>)> {
        Ok((
            normalise_locale("hl", self.hl.clone())?,
            normalise_locale("gl", self.gl.clone())?,
        ))
    }
}

/// Query parameters accepted by the two listing endpoints.
#[derive(Debug, Default, Deserialize)]
pub struct ListingQuery {
    pub limit: Option<u16>,
    pub cursor: Option<String>,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

impl ListingQuery {
    fn page(&self) -> PageParams {
        PageParams {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }

    fn locales(&self) -> vydra_errors::Result<(Option<String>, Option<String>)> {
        Ok((
            normalise_locale("hl", self.hl.clone())?,
            normalise_locale("gl", self.gl.clone())?,
        ))
    }
}

/// Locales are interpolated into provider query strings and cache keys, so they are validated here
/// (finding A-11): ASCII letters/digits plus `-`, 2..=10 chars, lowercased.
fn normalise_locale(
    field: &'static str,
    raw: Option<String>,
) -> vydra_errors::Result<Option<String>> {
    let Some(value) = raw else { return Ok(None) };
    let trimmed = value.trim();
    let valid = (2..=10).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(Error::validation(field, "invalid_locale"));
    }
    Ok(Some(trimmed.to_ascii_lowercase()))
}

fn parse_features(raw: Option<&str>) -> vydra_errors::Result<Vec<SearchFeature>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut features = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let feature = match part {
            "live" => SearchFeature::Live,
            "hd" => SearchFeature::Hd,
            "4k" => SearchFeature::Uhd4k,
            "subtitles" => SearchFeature::Subtitles,
            "creative_commons" => SearchFeature::CreativeCommons,
            "3d" => SearchFeature::ThreeD,
            "360" => SearchFeature::ThreeSixty,
            "vr180" => SearchFeature::Vr180,
            "hdr" => SearchFeature::Hdr,
            _ => return Err(Error::validation("filter", "unsupported")),
        };
        if !features.contains(&feature) {
            features.push(feature);
        }
    }
    Ok(features)
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
    let filters_allowed = state.config.providers.enable_unverified_filters;
    let input = query
        .into_input(kind, filters_allowed)
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let results = state
        .engines
        .search(input)
        .instrument(tracing::info_span!(
            "engine.search",
            kind = kind.cursor_namespace()
        ))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        results.items.len(),
        results.next_cursor.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &results.provenance, Some(pagination));
    Ok(Json(Envelope {
        data: results,
        meta,
    }))
}

async fn video_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<VideoQuery>,
) -> ApiResult<Json<Envelope<VideoDocument>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let input = query
        .into_input(video_id)
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let document = state
        .engines
        .video(input)
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &document.provenance, None);
    Ok(Json(Envelope {
        data: document,
        meta,
    }))
}

async fn chapters_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<VideoQuery>,
) -> ApiResult<Json<Envelope<vydra_domain::VideoChapters>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let input = query
        .into_input(video_id)
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let chapters = state
        .engines
        .chapters(input)
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &chapters.provenance, None);
    Ok(Json(Envelope {
        data: chapters,
        meta,
    }))
}

async fn captions_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<VideoQuery>,
) -> ApiResult<Json<Envelope<vydra_domain::CaptionTrackList>>> {
    let video_id = VideoId::parse(id).map_err(|e| ApiError::new(e, &request_id.0))?;
    let input = query
        .into_input(video_id)
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let tracks = state
        .engines
        .captions(input)
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &tracks.provenance, None);
    Ok(Json(Envelope { data: tracks, meta }))
}

// --- middleware -----------------------------------------------------------------------------

// --- Phase 6: channels and playlists ---------------------------------------------------------

/// `GET /api/v1/channels/{id}` — `id` is a `UC…` channel id.
///
/// The reference is validated *before* any provider call: a syntactically invalid id is a 400 and never
/// reaches the registry, and a `@handle` is refused with `400 validation_failed{id: handle_not_supported}`
/// because upstream does not accept a handle as a browse id (probe-verified, Phase 6).
async fn channel_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ResourceQuery>,
) -> ApiResult<Json<Envelope<ChannelDocument>>> {
    let reference = ChannelRef::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    // Handles are refused rather than resolved: upstream does not accept a handle as a browse id (the
    // Phase 6 probe received `400`), and a channel reached through a search is a guess about which channel
    // the caller meant. `docs/research/CHANNEL_CONTRACT.md` §7 records the evidence.
    if matches!(reference, ChannelRef::Handle(_)) {
        let error = Error::validation("id", "handle_not_supported");
        return Err(ApiError::new(error, &request_id.0));
    }
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let document = state
        .engines
        .channel(ChannelInput { reference, hl, gl })
        .instrument(tracing::info_span!("engine.channel"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &document.provenance, None);
    Ok(Json(Envelope {
        data: document,
        meta,
    }))
}

/// `GET /api/v1/channels/{id}/videos`.
async fn channel_videos_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ListingQuery>,
) -> ApiResult<Json<Envelope<ChannelVideoList>>> {
    let channel_id =
        vydra_common::ChannelId::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let limit = query.limit;
    let list = state
        .engines
        .channel_videos(ChannelVideosInput {
            id: channel_id,
            page: query.page(),
            hl,
            gl,
        })
        .instrument(tracing::info_span!("engine.channel.videos"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        list.items.len(),
        list.next_cursor.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &list.provenance, Some(pagination));
    Ok(Json(Envelope { data: list, meta }))
}

/// `GET /api/v1/videos/{id}/comments`.
///
/// Read-only. The order is one of the two upstream offers, the cursor is opaque and signed, and the
/// envelope states how many replies came with each thread, how many exist in total, and whether more pages
/// are available.
async fn comments_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<CommentsQuery>,
) -> ApiResult<Json<Envelope<CommentList>>> {
    let video_id = VideoId::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let sort = query.sort().map_err(|e| ApiError::new(e, &request_id.0))?;
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let limit = query.limit;
    let list = state
        .engines
        .comments(CommentsInput {
            id: video_id,
            sort,
            page: query.page(),
            hl,
            gl,
        })
        .instrument(tracing::info_span!("engine.comments"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        list.items.len(),
        list.next_token.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &list.provenance, Some(pagination));
    Ok(Json(Envelope { data: list, meta }))
}

/// `GET /api/v1/videos/{id}/comments/{comment_id}/replies`.
///
/// The parent comment id is validated before any upstream call, and it is part of the cursor's namespace:
/// a cursor minted for one thread is rejected by another.
async fn comment_replies_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path((id, comment_id)): Path<(String, String)>,
    Query(query): Query<RepliesQuery>,
) -> ApiResult<Json<Envelope<ReplyList>>> {
    let video_id = VideoId::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let parent = CommentId::parse(comment_id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let limit = query.limit;
    let replies = state
        .engines
        .comment_replies(CommentRepliesInput {
            id: video_id,
            parent,
            page: query.page(),
            hl,
            gl,
        })
        .instrument(tracing::info_span!("engine.comments.replies"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        replies.items.len(),
        replies.next_token.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &replies.provenance, Some(pagination));
    Ok(Json(Envelope { data: replies, meta }))
}

/// `GET /api/v1/playlists/{id}`.
async fn playlist_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ResourceQuery>,
) -> ApiResult<Json<Envelope<PlaylistDocument>>> {
    let playlist_id = PlaylistId::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let document = state
        .engines
        .playlist(PlaylistInput {
            id: playlist_id,
            hl,
            gl,
        })
        .instrument(tracing::info_span!("engine.playlist"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let meta = Meta::from_provenance(&request_id.0, &document.provenance, None);
    Ok(Json(Envelope {
        data: document,
        meta,
    }))
}

/// `GET /api/v1/playlists/{id}/videos`.
async fn playlist_videos_handler(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ListingQuery>,
) -> ApiResult<Json<Envelope<PlaylistVideoList>>> {
    let playlist_id = PlaylistId::parse(id.trim()).map_err(|e| ApiError::new(e, &request_id.0))?;
    let (hl, gl) = query
        .locales()
        .map_err(|e| ApiError::new(e, &request_id.0))?;
    let limit = query.limit;
    let list = state
        .engines
        .playlist_videos(PlaylistVideosInput {
            id: playlist_id,
            page: query.page(),
            hl,
            gl,
        })
        .instrument(tracing::info_span!("engine.playlist.videos"))
        .await
        .map_err(|e| ApiError::new(e, &request_id.0))?;

    let pagination = PaginationMeta::new(
        limit.unwrap_or(vydra_common::DEFAULT_LIMIT),
        list.items.len(),
        list.next_cursor.clone(),
    );
    let meta = Meta::from_provenance(&request_id.0, &list.provenance, Some(pagination));
    Ok(Json(Envelope { data: list, meta }))
}

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

    // Anonymous callers are bucketed per peer IP when the socket address is available, so one
    // client cannot exhaust the shared anonymous budget (finding A-09). Behind a reverse proxy all
    // traffic appears to come from the proxy unless the proxy is trusted to inject the peer
    // address; see docs/security/PROVIDER_ENGINE_AUDIT.md "unsolvable limitations".
    let bucket = if principal.anonymous {
        match request.extensions().get::<ConnectInfo<SocketAddr>>() {
            Some(ConnectInfo(addr)) => format!("ip:{}", addr.ip()),
            None => "anonymous".to_owned(),
        }
    } else {
        format!("key:{}", principal.key_id)
    };
    if let Err(error) = state.limiter.check(&bucket, state.config.auth.per_key_rpm) {
        state.metrics.record_quota_denied("per_key_rpm");
        return ApiError::new(error, &request_id).into_response();
    }

    request.extensions_mut().insert(principal);
    next.run(request).await
}

// --- metrics bridge -------------------------------------------------------------------------

/// Bridges provider lifecycle events into Prometheus counters.
#[derive(Debug)]
pub struct MetricsObserver {
    metrics: Arc<Metrics>,
}

impl MetricsObserver {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }
}

impl ProviderObserver for MetricsObserver {
    fn on_attempt(
        &self,
        provider: ProviderId,
        capability: Capability,
        outcome: AttemptOutcome,
        latency: Duration,
    ) {
        let outcome = match outcome {
            AttemptOutcome::Ok => "ok",
            AttemptOutcome::Empty => "empty",
            AttemptOutcome::Failed => "failed",
            AttemptOutcome::Skipped => "skipped",
        };
        self.metrics.record_provider_attempt(
            provider.as_str(),
            capability.as_str(),
            outcome,
            latency,
        );
    }

    fn on_circuit_change(&self, provider: ProviderId, state: CircuitState) {
        let label = match state {
            CircuitState::Closed => "closed",
            CircuitState::HalfOpen => "half_open",
            CircuitState::Open { .. } => "open",
        };
        self.metrics.record_circuit_state(provider.as_str(), label);
    }
}

// --- router ---------------------------------------------------------------------------------

/// Build the router. Endpoints whose capability is not declared by any provider are **not mounted**
/// (`docs/architecture/ARCHITECTURE.md` §5).
///
/// Phase 6 mounts four channel/playlist endpoints under the same rule: `/channels/{id}`, its `/videos`
/// listing, `/playlists/{id}` and its `/videos` listing. `ChannelPlaylists`, `ChannelAbout` and the
/// channel tabs other than `videos` are *not* mounted because no provider declares them.
///
/// Phase 7 adds the two comment routes, each behind its own capability: `/videos/{id}/comments`
/// (`CommentsThread`) and `/videos/{id}/comments/{comment_id}/replies` (`CommentsReplies`).
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
    // Phase 6. Mounted only when a provider declares the capability: an endpoint that would answer
    // `capability_unavailable` for every request is worse than a 404, because it suggests the resource
    // exists and the server is merely busy.
    if capabilities.contains(Capability::ChannelMetadata) {
        api = api.route("/channels/{id}", get(channel_handler));
    }
    if capabilities.contains(Capability::ChannelVideos) {
        api = api.route("/channels/{id}/videos", get(channel_videos_handler));
    }
    if capabilities.contains(Capability::PlaylistMetadata) {
        api = api.route("/playlists/{id}", get(playlist_handler));
    }
    if capabilities.contains(Capability::PlaylistItems) {
        api = api.route("/playlists/{id}/videos", get(playlist_videos_handler));
    }
    // Phase 7. Two routes, two capabilities: a provider that can read a comment section is not automatically
    // one that can find a reply continuation, and mounting both on one capability would serve an endpoint that
    // fails for a reason the caller cannot see.
    if capabilities.contains(Capability::CommentsThread) {
        api = api.route("/videos/{id}/comments", get(comments_handler));
    }
    if capabilities.contains(Capability::CommentsReplies) {
        api = api.route(
            "/videos/{id}/comments/{comment_id}/replies",
            get(comment_replies_handler),
        );
    }

    // Ops endpoints stay reachable without an API key so orchestrators can probe the service
    // (finding A-12: previously every route, including /health, required authentication).
    let ops = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/status", get(status))
        .route("/metrics", get(metrics));

    let authed = api.layer(middleware::from_fn_with_state(
        state.clone(),
        auth_middleware,
    ));

    Router::new()
        .merge(ops.clone())
        .nest("/api/v1", authed.merge(ops))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            Duration::from_millis(state.config.server.request_timeout_ms),
        ))
        .layer(RequestBodyLimitLayer::new(
            state.config.server.body_limit_bytes,
        ))
        .with_state(state)
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
            sort: None,
            since: None,
            duration: None,
            filter: None,
        };
        assert!(matches!(
            query.into_input(SearchKind::All, false),
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
            sort: None,
            since: None,
            duration: None,
            filter: None,
        };
        assert!(query.into_input(SearchKind::All, false).is_err());
    }

    fn simple_query() -> SearchQuery {
        SearchQuery {
            q: "rust".into(),
            limit: None,
            cursor: None,
            hl: None,
            gl: None,
            sort: None,
            since: None,
            duration: None,
            filter: None,
        }
    }

    #[test]
    fn filters_require_opt_in() {
        let query = SearchQuery {
            sort: Some("view_count".into()),
            ..simple_query()
        };
        assert!(matches!(
            query.into_input(SearchKind::Videos, false),
            Err(Error::CapabilityUnavailable(_))
        ));

        let query = SearchQuery {
            sort: Some("view_count".into()),
            ..simple_query()
        };
        let input = query.into_input(SearchKind::Videos, true).expect("input");
        assert_eq!(input.filters.sort, SearchSort::ViewCount);
    }

    #[test]
    fn malformed_filter_values_are_rejected() {
        for query in [
            SearchQuery {
                sort: Some("views".into()),
                ..simple_query()
            },
            SearchQuery {
                since: Some("decade".into()),
                ..simple_query()
            },
            SearchQuery {
                duration: Some("eternal".into()),
                ..simple_query()
            },
            SearchQuery {
                filter: Some("live,teleport".into()),
                ..simple_query()
            },
        ] {
            assert!(query.into_input(SearchKind::All, true).is_err());
        }
    }

    #[test]
    fn feature_lists_are_deduplicated() {
        let query = SearchQuery {
            filter: Some("live, hd,live".into()),
            ..simple_query()
        };
        let input = query.into_input(SearchKind::All, true).expect("input");
        assert_eq!(
            input.filters.features,
            vec![SearchFeature::Live, SearchFeature::Hd]
        );
    }

    #[test]
    fn locales_are_validated_and_normalised() {
        let query = SearchQuery {
            hl: Some("EN-us".into()),
            ..simple_query()
        };
        assert_eq!(
            query
                .into_input(SearchKind::All, false)
                .expect("input")
                .hl
                .as_deref(),
            Some("en-us")
        );
        let query = SearchQuery {
            hl: Some("en us".into()),
            ..simple_query()
        };
        assert!(query.into_input(SearchKind::All, false).is_err());
        let query = SearchQuery {
            gl: Some("x".into()),
            ..simple_query()
        };
        assert!(query.into_input(SearchKind::All, false).is_err());
    }

    #[test]
    fn video_query_validates_and_normalises_locales() {
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");

        let input = VideoQuery {
            hl: Some("DE-de".into()),
            gl: Some("de".into()),
        }
        .into_input(id.clone())
        .expect("valid locales");
        assert_eq!(input.hl.as_deref(), Some("de-de"));
        assert_eq!(input.gl.as_deref(), Some("de"));
        assert_eq!(input.id, id);

        // Absent means "use the deployment default", which the engine resolves.
        let input = VideoQuery::default().into_input(id.clone()).expect("input");
        assert!(input.hl.is_none() && input.gl.is_none());

        // The same validation the search endpoint applies: locales reach the provider and the cache key.
        assert!(
            VideoQuery {
                hl: Some("en us".into()),
                ..Default::default()
            }
            .into_input(id.clone())
            .is_err()
        );
        assert!(
            VideoQuery {
                gl: Some("x".into()),
                ..Default::default()
            }
            .into_input(id)
            .is_err()
        );
    }

    #[test]
    fn a_comment_order_is_validated_not_dropped() {
        // Absent means the default order, which is the one every implementation sends unprompted.
        assert_eq!(
            CommentsQuery::default().sort().expect("default"),
            CommentSort::Top
        );
        assert_eq!(
            CommentsQuery {
                sort: Some("top".into()),
                ..Default::default()
            }
            .sort()
            .expect("top"),
            CommentSort::Top
        );
        assert_eq!(
            CommentsQuery {
                sort: Some("newest".into()),
                ..Default::default()
            }
            .sort()
            .expect("newest"),
            CommentSort::Newest
        );

        // Anything else is a caller error. Silently serving the default order would answer a question the
        // caller did not ask, and there is no way for the response to say it did.
        for bad in ["views", "oldest", "TOP", "", "newest,top"] {
            let error = CommentsQuery {
                sort: Some(bad.to_owned()),
                ..Default::default()
            }
            .sort()
            .expect_err("must reject");
            assert!(
                matches!(error, Error::Validation(_)),
                "expected a validation error for {bad:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn comment_locales_are_validated_like_every_other_endpoint() {
        let query = CommentsQuery {
            hl: Some("EN-us".into()),
            gl: Some("us".into()),
            ..Default::default()
        };
        let (hl, gl) = query.locales().expect("locales");
        assert_eq!(hl.as_deref(), Some("en-us"));
        assert_eq!(gl.as_deref(), Some("us"));

        assert!(
            CommentsQuery {
                hl: Some("en us".into()),
                ..Default::default()
            }
            .locales()
            .is_err()
        );
        // The reply endpoint takes the same dimensions through its own type, because a handler can only hold
        // one `Query`.
        assert!(
            RepliesQuery {
                gl: Some("usa".into()),
                ..Default::default()
            }
            .locales()
            .is_err()
        );
    }

    #[test]
    fn comments_disabled_is_a_conflict_with_a_named_reason() {
        let error = Error::ContentUnavailable {
            reason: vydra_errors::UnavailabilityReason::CommentsDisabled,
            message: "watch page has no comment section".into(),
        };
        let body = ErrorBody::from_error(&error, "rid");
        assert_eq!(body.status, 409);
        assert_eq!(body.code, vydra_errors::ErrorCode::ContentUnavailable);
        assert_eq!(body.details.len(), 1);
        assert_eq!(body.details[0].field, "availability");
        assert_eq!(body.details[0].issue, "comments_disabled");
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
            sources: Vec::new(),
        };
        let meta = Meta::from_provenance("rid", &provenance, None);
        assert_eq!(meta.request_id, "rid");
        assert_eq!(meta.provider.as_deref(), Some("innertube-web"));
        let principal = Principal::anonymous();
        let json = serde_json::to_string(&meta).expect("serialize");
        assert!(!json.contains(&principal.key_id) || principal.key_id == "anonymous");
    }
}
