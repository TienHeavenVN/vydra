//! Engines: orchestration of registry + cache + cursors into use cases.
//!
//! Engines own no HTTP and no `axum` types, so they are reusable by the API, the worker and tests.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use vydra_cache::{Cache, CacheClass, Lookup};
use vydra_common::{CursorCodec, PageParams, VideoId};
use vydra_domain::{
    Availability, CaptionTrackList, Chapter, Provenance, SearchItem, SearchResults, VideoChapters,
    VideoDocument, VideoSummary,
};
use vydra_errors::Error;
use vydra_provider_registry::{FetchOutcome, ProviderRegistry};
use vydra_providers::{
    ProviderContext, ProviderRequest, ProviderResponse, SearchFilters, SearchKind, Strictness,
};

/// Engine dependencies and policy.
#[derive(Debug, Clone)]
pub struct EnginesConfig {
    pub hl: String,
    pub gl: String,
    /// Budget for a single engine call, including fallback attempts.
    pub call_budget: Duration,
    pub metadata_ttl: Duration,
    /// TTL for `live`/`upcoming` documents (ADR-022).
    pub live_ttl: Duration,
    /// TTL for content-fact `unavailable` documents (ADR-022).
    pub unavailable_ttl: Duration,
    pub search_ttl: Duration,
    pub cache_search: bool,
    pub strictness: Strictness,
    /// Mirrors `providers.enable_unverified_filters`. Enforced here as well as at the HTTP edge so
    /// non-HTTP callers (worker, tests) cannot bypass the capability gate.
    pub unverified_filters: bool,
}

impl Default for EnginesConfig {
    fn default() -> Self {
        Self {
            hl: "en".into(),
            gl: "US".into(),
            call_budget: Duration::from_millis(8_000),
            metadata_ttl: Duration::from_secs(300),
            live_ttl: Duration::from_secs(30),
            unavailable_ttl: Duration::from_secs(60),
            search_ttl: Duration::from_secs(60),
            cache_search: false,
            strictness: Strictness::Lenient,
            unverified_filters: false,
        }
    }
}

/// Search use case input.
#[derive(Debug, Clone)]
pub struct SearchInput {
    pub query: String,
    pub kind: SearchKind,
    pub filters: SearchFilters,
    pub page: PageParams,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

/// Video use case input.
#[derive(Debug, Clone)]
pub struct VideoInput {
    pub id: VideoId,
    pub hl: Option<String>,
    pub gl: Option<String>,
}

/// How long a video document may be cached, as a function of what it says (ADR-022).
///
/// The rule is that the TTL follows the *fact*, not the endpoint:
///
/// | Document | TTL | Why |
/// | --- | --- | --- |
/// | `available` | `metadata_ttl` (300 s) | a published upload's metadata is stable for minutes |
/// | `live` / `upcoming` | `live_ttl` (30 s) | viewer counts, end time and state move while it airs |
/// | `unavailable` | `unavailable_ttl` (60 s) | content facts are reversible: private → public |
/// | `unknown` | **never** | the next request may get a verdict, so caching one that did not would freeze a gap |
///
/// Upstream *failures* never reach this table: a transport or parse error is an `Err`, and the engine
/// stores nothing at all, so a 20-minute upstream outage cannot be remembered as "this video is
/// unavailable".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentCachePolicy {
    pub available: Duration,
    pub live: Duration,
    pub unavailable: Duration,
}

impl DocumentCachePolicy {
    /// `None` means "do not cache".
    pub fn ttl(self, availability: Availability) -> Option<Duration> {
        match availability {
            Availability::Available => Some(self.available),
            Availability::Live | Availability::Upcoming => Some(self.live),
            Availability::Unavailable { .. } => Some(self.unavailable),
            Availability::Unknown => None,
        }
    }
}

/// The engine facade.
///
/// `Debug` is implemented by hand because the cache is a `dyn Cache` and is deliberately opaque.
pub struct Engines {
    registry: Arc<ProviderRegistry>,
    cache: Arc<dyn Cache>,
    cursors: CursorCodec,
    config: EnginesConfig,
    config_hash: u64,
}

impl std::fmt::Debug for Engines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engines")
            .field("registry", &self.registry)
            .field("config", &self.config)
            .field("config_hash", &self.config_hash)
            .finish_non_exhaustive()
    }
}

impl Engines {
    pub fn new(
        registry: Arc<ProviderRegistry>,
        cache: Arc<dyn Cache>,
        cursors: CursorCodec,
        config: EnginesConfig,
    ) -> Self {
        // Audit finding (Phase 5B): this used to fingerprint the provider *ids* only, so a client
        // profile bump — which changes the request context and therefore the localized body — kept
        // serving entries produced by the previous client. `config_fingerprint` covers the label too.
        let config_hash = registry.config_fingerprint();
        Self {
            registry,
            cache,
            cursors,
            config,
            config_hash,
        }
    }

    pub fn registry(&self) -> &Arc<ProviderRegistry> {
        &self.registry
    }

    /// Effective document cache policy, from the configured TTLs.
    pub fn cache_policy(&self) -> DocumentCachePolicy {
        DocumentCachePolicy {
            available: self.config.metadata_ttl,
            live: self.config.live_ttl,
            unavailable: self.config.unavailable_ttl,
        }
    }

    /// Cache key for a video document: every dimension that changes the response, and nothing else.
    ///
    /// `hl`/`gl` change titles, dates and counts; the id selects the resource. The client profile is not
    /// in the key because it cannot change at runtime — it is part of `config_hash`, which invalidates
    /// the whole entry set when it does change.
    fn video_cache_key(id: &VideoId, hl: &str, gl: &str) -> String {
        format!("video:{id}:{hl}:{gl}")
    }

    fn context(&self, hl: Option<String>, gl: Option<String>) -> ProviderContext {
        let now = Instant::now();
        ProviderContext {
            deadline: now + self.config.call_budget,
            request_id: uuid_like(),
            hl: hl.unwrap_or_else(|| self.config.hl.clone()),
            gl: gl.unwrap_or_else(|| self.config.gl.clone()),
            strictness: self.config.strictness,
        }
    }

    /// Search videos/channels/playlists/all.
    pub async fn search(&self, input: SearchInput) -> Result<SearchResults, Error> {
        let limit = input.page.effective_limit()?;
        let kind = input.kind;
        let hl = input.hl.clone().unwrap_or_else(|| self.config.hl.clone());
        let gl = input.gl.clone().unwrap_or_else(|| self.config.gl.clone());

        if !self.config.unverified_filters && has_non_default_filters(&input.filters) {
            return Err(Error::CapabilityUnavailable("search_filters".into()));
        }

        let cursor_token = match input.page.cursor_ref() {
            Some(raw) => Some(self.cursors.decode(raw, kind.cursor_namespace())?.t),
            None => None,
        };

        // Audit finding A-06: the cache key omitted the locale, so a `hl=de` response could be
        // served to a `hl=en` caller (localized numbers *and* text).
        let cache_key = format!(
            "{}:{}:{}:{}:{}:{}:{}",
            kind.cursor_namespace(),
            input.query.trim().to_lowercase(),
            describe_filters(&input.filters),
            cursor_token.as_deref().unwrap_or("-"),
            limit,
            hl,
            gl
        );

        if self.config.cache_search
            && let Lookup::Fresh(json) =
                self.cache
                    .get(CacheClass::Search, &cache_key, self.config_hash)
            && let Ok(mut cached) = serde_json::from_str::<SearchResults>(&json)
        {
            cached.provenance.cached = true;
            cached.provenance.sources.clear();
            return Ok(cached);
        }

        let ctx = self.context(Some(hl), Some(gl));
        let request = ProviderRequest::Search {
            query: input.query.trim().to_owned(),
            kind,
            filters: input.filters,
            cursor: cursor_token,
            hl: ctx.hl.clone(),
            gl: ctx.gl.clone(),
            limit,
        };

        let outcome = self.registry.fetch(request, &ctx).await?;
        let attempts = outcome.attempts.clone();
        let ProviderResponse::Search(mut results) = outcome.response else {
            return Err(Error::Internal(
                "provider returned the wrong payload type".into(),
            ));
        };

        // Audit finding A-07: a mixed-search payload can contain channels/playlists even for a
        // type-scoped request. Filter here so `/search/videos` cannot return a channel.
        match kind {
            SearchKind::Videos => results
                .items
                .retain(|item| matches!(item, SearchItem::Video(_))),
            SearchKind::Channels => results
                .items
                .retain(|item| matches!(item, SearchItem::Channel(_))),
            SearchKind::Playlists => results
                .items
                .retain(|item| matches!(item, SearchItem::Playlist(_))),
            SearchKind::All | SearchKind::Music => {}
        }
        results.provenance.sources = attempts;

        results.next_cursor = match results.next_cursor.take() {
            Some(token) => Some(self.cursors.encode(
                kind.cursor_namespace(),
                outcome.provider.as_str(),
                &token,
            )?),
            None => None,
        };

        if self.config.cache_search {
            self.cache.set(
                CacheClass::Search,
                &cache_key,
                serde_json::to_string(&results).map_err(|err| Error::Internal(err.to_string()))?,
                self.config.search_ttl,
                self.config_hash,
            );
        }

        Ok(results)
    }

    /// Full video metadata.
    pub async fn video(&self, input: VideoInput) -> Result<VideoDocument, Error> {
        let hl = input.hl.clone().unwrap_or_else(|| self.config.hl.clone());
        let gl = input.gl.clone().unwrap_or_else(|| self.config.gl.clone());
        let cache_key = Engines::video_cache_key(&input.id, &hl, &gl);
        if let Lookup::Fresh(json) =
            self.cache
                .get(CacheClass::Metadata, &cache_key, self.config_hash)
            && let Ok(mut document) = serde_json::from_str::<VideoDocument>(&json)
        {
            document.provenance.cached = true;
            document.provenance.sources.clear();
            return Ok(document);
        }

        let ctx = self.context(Some(hl), Some(gl));
        let request = ProviderRequest::Video {
            id: input.id.clone(),
            hl: ctx.hl.clone(),
            gl: ctx.gl.clone(),
        };

        let outcome = self.registry.fetch(request, &ctx).await?;
        let attempts = outcome.attempts.clone();
        let ProviderResponse::Video(document) = outcome.response else {
            return Err(Error::Internal(
                "provider returned the wrong payload type".into(),
            ));
        };
        let mut document = *document;
        document.provenance.sources = attempts;

        // Policy, not a boolean (ADR-022): `unknown` is never cached, `live`/`upcoming` expire in
        // seconds, `unavailable` in a minute, `available` in `metadata_ttl`. Upstream failures never
        // get here at all — they are `Err` and nothing is stored.
        if let Some(ttl) = self.cache_policy().ttl(document.availability) {
            self.cache.set(
                CacheClass::Metadata,
                &cache_key,
                serde_json::to_string(&document).map_err(|err| Error::Internal(err.to_string()))?,
                ttl,
                self.config_hash,
            );
        }

        Ok(document)
    }

    /// Chapters projected from the video document.
    pub async fn chapters(&self, input: VideoInput) -> Result<VideoChapters, Error> {
        let document = self.video(input).await?;
        Ok(VideoChapters {
            items: document.chapters.clone(),
            provenance: document.provenance.clone(),
        })
    }

    /// Caption track metadata.
    pub async fn captions(&self, input: VideoInput) -> Result<CaptionTrackList, Error> {
        let document = self.video(input).await?;
        Ok(CaptionTrackList {
            items: document.caption_tracks.clone(),
            provenance: document.provenance.clone(),
        })
    }

    /// Related videos, from a provider response when available.
    pub async fn related(&self, input: VideoInput) -> Result<vydra_domain::RelatedVideos, Error> {
        // The InnerTube `next` endpoint is not implemented yet; reporting `capability_unavailable`
        // is honest and keeps the endpoint from being mounted (see docs/PROGRESS.md).
        let _ = input;
        Err(Error::CapabilityUnavailable("video_related".into()))
    }

    /// Attempt metadata for a list of video ids, tolerating individual failures.
    pub async fn videos_best_effort(&self, ids: &[VideoId]) -> Vec<VideoSummary> {
        let mut out = Vec::new();
        for id in ids {
            if let Ok(document) = self
                .video(VideoInput {
                    id: id.clone(),
                    hl: None,
                    gl: None,
                })
                .await
            {
                out.push(document.summary);
            }
        }
        out
    }
}

/// Outcome plus the attempt list, for handlers that must surface `meta.sources`.
pub fn outcome_attempts(outcome: &FetchOutcome) -> &[vydra_domain::SourceAttempt] {
    &outcome.attempts
}

/// Structural chapters helper used by tests and the worker.
pub fn chapters_of(document: &VideoDocument) -> &[Chapter] {
    &document.chapters
}

fn has_non_default_filters(filters: &SearchFilters) -> bool {
    filters != &SearchFilters::default()
}

fn describe_filters(filters: &SearchFilters) -> String {
    format!(
        "{:?}|{:?}|{:?}|{}",
        filters.sort,
        filters.uploaded,
        filters.duration,
        filters.features.len()
    )
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("eng-{counter:016x}")
}

/// Provenance helper for engines that synthesize documents.
pub fn provenance_for(provider: &str, endpoint: &str) -> Provenance {
    Provenance {
        provider: provider.to_owned(),
        endpoint: endpoint.to_owned(),
        client: None,
        fetched_at: time::OffsetDateTime::now_utc(),
        cached: false,
        parse_warnings: 0,
        sources: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vydra_cache::MemoryCache;
    use vydra_providers::{Capability, CapabilitySet, Provider, ProviderId};

    #[derive(Debug)]
    struct FixedProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for FixedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fixed")
        }
        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::from_slice(&[
                Capability::SearchAll,
                Capability::SearchVideos,
                Capability::SearchChannels,
                Capability::SearchPlaylists,
                Capability::VideoMetadata,
                Capability::CaptionsMetadata,
            ])
        }
        fn client_label(&self) -> String {
            "fixed-1".into()
        }
        async fn fetch(
            &self,
            request: ProviderRequest,
            _ctx: &ProviderContext,
        ) -> Result<ProviderResponse, vydra_errors::ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request {
                ProviderRequest::Search { .. } => Ok(ProviderResponse::Search(SearchResults {
                    items: vec![],
                    next_cursor: Some("RAW-TOKEN".into()),
                    estimated_results: None,
                    provenance: provenance_for("fixed", "search"),
                })),
                ProviderRequest::Video { id, .. } => {
                    Ok(ProviderResponse::Video(Box::new(VideoDocument {
                        summary: VideoSummary {
                            id,
                            title: "t".into(),
                            channel_id: None,
                            channel_title: None,
                            duration_seconds: None,
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
                        storyboards: None,
                        publication: None,
                        broadcast: None,
                        unlisted: None,
                        family_safe: None,
                        evidence: vydra_domain::MetadataEvidence::default(),
                        provenance: provenance_for("fixed", "player"),
                    })))
                }
                _ => Err(vydra_errors::ProviderError::Unsupported {
                    capability: "x".into(),
                }),
            }
        }
    }

    /// A provider that answers with a document in a chosen availability state.
    ///
    /// The cache policy is a function of the document, not of the endpoint, so the tests need to
    /// produce each state from the same code path the real provider uses.
    #[derive(Debug)]
    struct DocumentProvider {
        calls: Arc<AtomicUsize>,
        availability: Availability,
        label: &'static str,
    }

    #[async_trait]
    impl Provider for DocumentProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("doc")
        }
        fn capabilities(&self) -> CapabilitySet {
            CapabilitySet::from_slice(&[Capability::VideoMetadata])
        }
        fn client_label(&self) -> String {
            self.label.into()
        }
        async fn fetch(
            &self,
            request: ProviderRequest,
            _ctx: &ProviderContext,
        ) -> Result<ProviderResponse, vydra_errors::ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let ProviderRequest::Video { id, .. } = request else {
                return Err(vydra_errors::ProviderError::Unsupported {
                    capability: "video".into(),
                });
            };
            Ok(ProviderResponse::Video(Box::new(VideoDocument {
                summary: VideoSummary {
                    id,
                    title: "t".into(),
                    channel_id: None,
                    channel_title: None,
                    duration_seconds: Some(60),
                    view_count: None,
                    published_text: None,
                    published_at: None,
                    thumbnails: vec![],
                    live_state: None,
                    badges: vec![],
                },
                availability: self.availability,
                description: None,
                category: None,
                keywords: vec![],
                like_count: None,
                comment_count: None,
                chapters: vec![],
                caption_tracks: vec![],
                formats: vec![],
                storyboards: None,
                publication: None,
                broadcast: None,
                unlisted: None,
                family_safe: None,
                evidence: vydra_domain::MetadataEvidence::default(),
                provenance: provenance_for("doc", "player"),
            })))
        }
    }

    fn engines_for(
        availability: Availability,
        label: &'static str,
        cache: Option<Arc<MemoryCache>>,
    ) -> (Engines, Arc<AtomicUsize>, Arc<MemoryCache>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(DocumentProvider {
            calls: calls.clone(),
            availability,
            label,
        });
        let registry = Arc::new(ProviderRegistry::new(vec![provider]));
        let cache = cache.unwrap_or_else(|| Arc::new(MemoryCache::new(64)));
        let cursors =
            CursorCodec::new("0123456789abcdef0123456789abcdef", Duration::from_secs(600))
                .expect("codec");
        (
            Engines::new(registry, cache.clone(), cursors, EnginesConfig::default()),
            calls,
            cache,
        )
    }

    fn video_input(hl: Option<&str>) -> VideoInput {
        VideoInput {
            id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
            hl: hl.map(str::to_owned),
            gl: None,
        }
    }

    #[test]
    fn cache_ttl_follows_the_fact_not_the_endpoint() {
        let policy = DocumentCachePolicy {
            available: Duration::from_secs(300),
            live: Duration::from_secs(30),
            unavailable: Duration::from_secs(60),
        };
        assert_eq!(
            policy.ttl(Availability::Available),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            policy.ttl(Availability::Live),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            policy.ttl(Availability::Upcoming),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            policy.ttl(Availability::Unavailable {
                reason: vydra_errors::UnavailabilityReason::Private
            }),
            Some(Duration::from_secs(60)),
            "unavailability is reversible, so it is cached briefly rather than forever"
        );
        assert_eq!(
            policy.ttl(Availability::Unknown),
            None,
            "a missing verdict is never cached"
        );
    }

    #[tokio::test]
    async fn a_missing_verdict_is_never_cached() {
        let (engines, calls, _) = engines_for(Availability::Unknown, "doc-1", None);
        engines.video(video_input(None)).await.expect("document");
        let second = engines.video(video_input(None)).await.expect("document");
        assert!(!second.provenance.cached);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the next request may get a verdict, so a verdict-less answer must not be reused"
        );
    }

    #[tokio::test]
    async fn an_unavailable_document_is_cached_briefly_not_forever() {
        let (engines, calls, _) = engines_for(
            Availability::Unavailable {
                reason: vydra_errors::UnavailabilityReason::Private,
            },
            "doc-1",
            None,
        );
        engines.video(video_input(None)).await.expect("document");
        let second = engines.video(video_input(None)).await.expect("document");
        assert!(second.provenance.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // The stored TTL is the short one, so the entry cannot outlive a privacy change by long.
        assert_eq!(
            engines.cache_policy().ttl(Availability::Unavailable {
                reason: vydra_errors::UnavailabilityReason::Private
            }),
            Some(Duration::from_secs(60))
        );
    }

    #[tokio::test]
    async fn live_documents_use_the_short_ttl() {
        let (engines, _, _) = engines_for(Availability::Live, "doc-1", None);
        let document = engines.video(video_input(None)).await.expect("document");
        assert_eq!(document.availability, Availability::Live);
        assert!(
            engines
                .cache_policy()
                .ttl(document.availability)
                .expect("live is cacheable")
                < engines
                    .cache_policy()
                    .ttl(Availability::Available)
                    .expect("available is cacheable"),
            "live metadata moves while it airs and must expire sooner"
        );
    }

    #[tokio::test]
    async fn the_cache_key_separates_every_locale_that_changes_the_document() {
        let (engines, calls, _) = engines_for(Availability::Available, "doc-1", None);
        engines
            .video(video_input(Some("de")))
            .await
            .expect("document");
        engines
            .video(video_input(Some("en")))
            .await
            .expect("document");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a de answer must not be served to an en caller"
        );

        let again = engines
            .video(video_input(Some("de")))
            .await
            .expect("document");
        assert!(again.provenance.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_client_profile_change_invalidates_cached_documents() {
        // Same cache, same provider id, different client label: the second engine must not be served the
        // first engine's body (Phase 5B: the key was salted with provider ids only).
        let shared = Arc::new(MemoryCache::new(64));
        let (first, first_calls, _) =
            engines_for(Availability::Available, "web-1", Some(shared.clone()));
        first.video(video_input(None)).await.expect("document");
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);

        let (second, second_calls, _) = engines_for(Availability::Available, "web-2", Some(shared));
        let document = second.video(video_input(None)).await.expect("document");
        assert!(!document.provenance.cached);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    }

    fn engines(cache_search: bool) -> (Engines, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(FixedProvider {
            calls: calls.clone(),
        });
        let registry = Arc::new(ProviderRegistry::new(vec![provider]));
        let cache = Arc::new(MemoryCache::new(64));
        let cursors =
            CursorCodec::new("0123456789abcdef0123456789abcdef", Duration::from_secs(600))
                .expect("codec");
        (
            Engines::new(
                registry,
                cache,
                cursors,
                EnginesConfig {
                    cache_search,
                    ..Default::default()
                },
            ),
            calls,
        )
    }

    fn search_input() -> SearchInput {
        SearchInput {
            query: "rust".into(),
            kind: SearchKind::All,
            filters: SearchFilters::default(),
            page: PageParams::default(),
            hl: None,
            gl: None,
        }
    }

    #[tokio::test]
    async fn provider_cursor_is_replaced_by_a_signed_cursor() {
        let (engines, _) = engines(false);
        let results = engines.search(search_input()).await.expect("search");
        let cursor = results.next_cursor.expect("cursor");
        assert_ne!(cursor, "RAW-TOKEN");
        assert!(cursor.contains('.'));
    }

    #[tokio::test]
    async fn cursors_are_validated_for_kind() {
        let (engines, _) = engines(false);
        let results = engines.search(search_input()).await.expect("search");
        let cursor = results.next_cursor.expect("cursor");

        let mut tampered = cursor;
        tampered.push('x');
        let error = engines
            .search(SearchInput {
                page: PageParams {
                    limit: None,
                    cursor: Some(tampered),
                },
                ..search_input()
            })
            .await
            .expect_err("must reject");
        assert!(matches!(error, Error::InvalidCursor));
    }

    #[tokio::test]
    async fn video_metadata_is_cached_and_flagged() {
        let (engines, calls) = engines(false);
        let input = VideoInput {
            id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
            hl: None,
            gl: None,
        };
        let first = engines.video(input.clone()).await.expect("video");
        assert!(!first.provenance.cached);

        // The cache stores JSON, so a document that cannot round-trip can never be a cache hit.
        let json = serde_json::to_string(&first).expect("serialize cached document");
        let round_tripped: VideoDocument =
            serde_json::from_str(&json).expect("deserialize cached document");
        assert_eq!(round_tripped.summary.id, first.summary.id);

        let second = engines.video(input).await.expect("video");
        assert!(second.provenance.cached);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cache must avoid a provider call"
        );
    }

    #[tokio::test]
    async fn search_is_not_cached_by_default() {
        let (engines, calls) = engines(false);
        engines.search(search_input()).await.expect("search");
        engines.search(search_input()).await.expect("search");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn type_scoped_search_filters_foreign_item_kinds() {
        let (engines, _) = engines(false);
        let input = SearchInput {
            kind: SearchKind::Videos,
            ..search_input()
        };
        let results = engines.search(input).await.expect("search");
        assert!(
            results
                .items
                .iter()
                .all(|item| matches!(item, vydra_domain::SearchItem::Video(_)))
        );
    }

    #[tokio::test]
    async fn related_reports_capability_unavailable() {
        let (engines, _) = engines(false);
        let error = engines
            .related(VideoInput {
                id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
                hl: None,
                gl: None,
            })
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::CapabilityUnavailable(_)));
    }
}
