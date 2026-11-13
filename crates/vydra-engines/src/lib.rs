//! Engines: orchestration of registry + cache + cursors into use cases.
//!
//! Engines own no HTTP and no `axum` types, so they are reusable by the API, the worker and tests.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use vydra_cache::{Cache, CacheClass, Lookup};
use vydra_common::{CursorCodec, PageParams, VideoId};
use vydra_domain::{
    CaptionTrackList, Chapter, Provenance, SearchResults, VideoChapters, VideoDocument,
    VideoSummary,
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
    pub search_ttl: Duration,
    pub cache_search: bool,
    pub strictness: Strictness,
}

impl Default for EnginesConfig {
    fn default() -> Self {
        Self {
            hl: "en".into(),
            gl: "US".into(),
            call_budget: Duration::from_millis(8_000),
            metadata_ttl: Duration::from_secs(300),
            search_ttl: Duration::from_secs(60),
            cache_search: false,
            strictness: Strictness::Lenient,
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

/// The engine facade.
#[derive(Debug)]
pub struct Engines {
    registry: Arc<ProviderRegistry>,
    cache: Arc<dyn Cache>,
    cursors: CursorCodec,
    config: EnginesConfig,
    config_hash: u64,
}

impl Engines {
    pub fn new(
        registry: Arc<ProviderRegistry>,
        cache: Arc<dyn Cache>,
        cursors: CursorCodec,
        config: EnginesConfig,
    ) -> Self {
        let config_hash = fingerprint(&registry.provider_ids());
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

        let cursor_token = match input.page.cursor_ref() {
            Some(raw) => Some(self.cursors.decode(raw, kind.cursor_namespace())?.t),
            None => None,
        };

        let cache_key = format!(
            "{}:{}:{}:{}:{}",
            kind.cursor_namespace(),
            input.query.trim().to_lowercase(),
            describe_filters(&input.filters),
            cursor_token.as_deref().unwrap_or("-"),
            limit
        );

        if self.cache_search {
            if let Lookup::Fresh(json) = self.cache.get(CacheClass::Search, &cache_key, self.config_hash) {
                if let Ok(mut cached) = serde_json::from_str::<SearchResults>(&json) {
                    cached.provenance.cached = true;
                    return Ok(cached);
                }
            }
        }

        let ctx = self.context(input.hl, input.gl);
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
        let ProviderResponse::Search(mut results) = outcome.response else {
            return Err(Error::Internal("provider returned the wrong payload type".into()));
        };

        results.next_cursor = match results.next_cursor.take() {
            Some(token) => Some(self.cursors.encode(
                kind.cursor_namespace(),
                outcome.provider.as_str(),
                &token,
            )?),
            None => None,
        };

        if self.cache_search {
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
        let cache_key = format!("video:{}", input.id);
        if let Lookup::Fresh(json) = self
            .cache
            .get(CacheClass::Metadata, &cache_key, self.config_hash)
        {
            if let Ok(mut document) = serde_json::from_str::<VideoDocument>(&json) {
                document.provenance.cached = true;
                return Ok(document);
            }
        }

        let ctx = self.context(input.hl, input.gl);
        let request = ProviderRequest::Video {
            id: input.id.clone(),
            hl: ctx.hl.clone(),
            gl: ctx.gl.clone(),
        };

        let outcome = self.registry.fetch(request, &ctx).await?;
        let ProviderResponse::Video(document) = outcome.response else {
            return Err(Error::Internal("provider returned the wrong payload type".into()));
        };

        // Unavailable content is never cached: availability can change in both directions.
        if document.availability.is_available() {
            self.cache.set(
                CacheClass::Metadata,
                &cache_key,
                serde_json::to_string(&document).map_err(|err| Error::Internal(err.to_string()))?,
                self.config.metadata_ttl,
                self.config_hash,
            );
        }

        Ok(*document)
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

fn describe_filters(filters: &SearchFilters) -> String {
    format!(
        "{:?}|{:?}|{:?}|{}",
        filters.sort,
        filters.uploaded,
        filters.duration,
        filters.features.len()
    )
}

fn fingerprint(providers: &[vydra_providers::ProviderId]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for provider in providers {
        for byte in provider.as_str().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vydra_cache::MemoryCache;
    use vydra_providers::{
        Capability, CapabilitySet, Provider, ProviderId,
    };

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
                ProviderRequest::Video { id, .. } => Ok(ProviderResponse::Video(Box::new(
                    VideoDocument {
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
                        provenance: provenance_for("fixed", "player"),
                    },
                ))),
                _ => Err(vydra_errors::ProviderError::Unsupported {
                    capability: "x".into(),
                }),
            }
        }
    }

    fn engines(cache_search: bool) -> (Engines, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(FixedProvider {
            calls: calls.clone(),
        });
        let registry = Arc::new(ProviderRegistry::new(vec![provider]));
        let cache = Arc::new(MemoryCache::new(64));
        let cursors = CursorCodec::new("0123456789abcdef0123456789abcdef", Duration::from_secs(600))
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
        let second = engines.video(input).await.expect("video");
        assert!(second.provenance.cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "cache must avoid a provider call");
    }

    #[tokio::test]
    async fn search_is_not_cached_by_default() {
        let (engines, calls) = engines(false);
        engines.search(search_input()).await.expect("search");
        engines.search(search_input()).await.expect("search");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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
