//! The InnerTube provider implementation.

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use time::OffsetDateTime;
use vydra_common::{ChannelId, ChannelRef, CommentId, PlaylistId, VideoId};
use vydra_domain::{
    CaptionTrackList, ChannelDocument, ChannelVideoList, CommentList, CommentSort, PlaylistDocument,
    PlaylistVideoList, Provenance, ReplyList, SearchResults, VideoDocument,
};
use vydra_errors::ProviderError;

use super::browse_map::{map_channel, map_channel_videos, map_playlist, map_playlist_videos};
use super::client::InnertubeClient;
use super::comments;
use super::map::{map_player, map_search};
use super::params::encode_search_filters;
use super::request::{
    BrowseRequest, ContinuationRequest, NextRequest, PlayerRequest, SearchRequest,
};
use super::{ClientKind, ClientProfile, InnertubeProviderConfig};
use crate::parse::{ParseReport, collect_renderers};
use crate::transport::Transport;
use crate::{
    Capability, CapabilitySet, Provider, ProviderContext, ProviderId, ProviderRequest,
    ProviderResponse, SearchFilters, SearchKind,
};

/// Rows requested from a playlist page when the caller has not stated a page size.
///
/// The playlist *document* embeds the first page of entries, so it needs a bound that is independent of
/// the caller's `limit`; `PlaylistVideoList` is trimmed to the caller's limit afterwards by the engine.
const PAGE_ROWS: u16 = 100;

/// How many comment pages a reply lookup may scan before giving up.
///
/// A reply continuation token exists only inside the comment page that rendered its thread, so the first
/// call for a thread's replies has to find that thread. Three pages is a cost bound (each page is a real
/// request), not a claim about where the thread is.
const REPLY_SCAN_PAGES: usize = 3;

/// InnerTube-backed provider.
#[derive(Debug)]
pub struct InnertubeProvider {
    config: Arc<InnertubeProviderConfig>,
    client: InnertubeClient,
    capabilities: CapabilitySet,
}

impl InnertubeProvider {
    pub fn new(config: Arc<InnertubeProviderConfig>, transport: Arc<dyn Transport>) -> Self {
        // Mixed search works without any `params` encoding, so it is declared unconditionally.
        // Type-scoped search and filter/sort require the protobuf `params` builder, which is *not*
        // verified against the live API yet (`params.rs`), so those capabilities are only declared
        // when the operator opts in. This keeps `/search/videos` from being mounted on unverified
        // behaviour (requirement 7 + docs/PROGRESS.md).
        let mut capabilities = CapabilitySet::from_slice(&[
            Capability::SearchAll,
            Capability::VideoMetadata,
            Capability::VideoChapters,
            Capability::VideoFormatsMetadata,
            Capability::VideoSubtitlesMetadata,
            Capability::CaptionsMetadata,
            // Deliberately *not* declared: `ChannelPlaylists` and `ChannelAbout` (those tabs are not
            // requested yet), `LiveMetadata` and `LiveChatMetadata`. Undeclared capabilities keep their
            // endpoints unmounted (`router()`), which is the honest alternative to serving a page this
            // parser does not read.
            Capability::ChannelMetadata,
            Capability::ChannelVideos,
            Capability::PlaylistMetadata,
            Capability::PlaylistItems,
            // Phase 7. Comments: the section token is relayed from the watch page, the sort tokens from the
            // page's own sort menu, and the reply tokens from the thread itself — three relayed contracts,
            // all live-verified by the Phase 7 probe (`docs/research/COMMENTS_REQUEST_CONTRACT.md`).
            Capability::CommentsThread,
            Capability::CommentsReplies,
            Capability::CursorPagination,
            Capability::LocaleSensitive,
        ]);
        if config.enable_unverified_filters {
            capabilities = capabilities
                .with(Capability::SearchFilters)
                .with(Capability::SearchVideos)
                .with(Capability::SearchChannels)
                .with(Capability::SearchPlaylists);
        }

        let client = InnertubeClient::new(config.clone(), transport);
        Self {
            config,
            client,
            capabilities,
        }
    }

    /// Convenience constructor using the default WEB profile.
    pub fn web(hl: &str, gl: &str, transport: Arc<dyn Transport>) -> Self {
        let config = InnertubeProviderConfig {
            hl: hl.to_owned(),
            gl: gl.to_owned(),
            ..Default::default()
        };
        Self::new(Arc::new(config), transport)
    }

    pub fn with_client(mut self, kind: ClientKind) -> Self {
        let mut config = (*self.config).clone();
        config.client = ClientProfile::from_kind(kind, None);
        config.provider_id = ProviderId::new(match kind {
            ClientKind::Web => "innertube-web",
            ClientKind::MWeb => "innertube-mweb",
            ClientKind::Tv => "innertube-tv",
        });
        self.config = Arc::new(config);
        self
    }

    pub fn profile(&self) -> &ClientProfile {
        self.client.profile()
    }

    fn provenance(
        &self,
        endpoint: &str,
        report: &ParseReport,
        fetched_at: OffsetDateTime,
    ) -> Provenance {
        Provenance {
            provider: self.config.provider_id.as_str().to_owned(),
            endpoint: endpoint.to_owned(),
            client: Some(self.client.profile().label()),
            fetched_at,
            cached: false,
            parse_warnings: report.count(),
            sources: Vec::new(),
        }
    }

    async fn search(
        &self,
        query: String,
        kind: SearchKind,
        filters: SearchFilters,
        cursor: Option<String>,
        ctx: &ProviderContext,
        limit: u16,
    ) -> Result<SearchResults, ProviderError> {
        if kind != SearchKind::All && !self.capabilities.contains(kind.required_capability()) {
            return Err(ProviderError::Unsupported {
                capability: kind.required_capability().as_str().to_owned(),
            });
        }
        let has_filters = filters != SearchFilters::default();
        if has_filters && !self.capabilities.contains(Capability::SearchFilters) {
            return Err(ProviderError::Unsupported {
                capability: Capability::SearchFilters.as_str().to_owned(),
            });
        }
        let params = encode_search_filters(&filters)?;

        // Continuation pages carry only the token; the query is not repeated. This is not a reading
        // of the reference clients any more: the Phase 4.3 probe sent exactly this body
        // (`{context, continuation}`) for the token taken from the first page and upstream answered
        // 200 with a complete second page (run 43541388070, capture `search_page_2`, 262,854 bytes).
        // The body type encodes it structurally: `ContinuationRequest` has no `query` field to
        // forget about, and none of the carried-in flags can leak into it.
        let session = self.client.session(ctx).await;
        let context = self.client.context(&session, ctx);
        let body = match cursor {
            Some(token) => serialize_body(&ContinuationRequest::new(context, token))?,
            None => match params {
                Some(params) => {
                    serialize_body(&SearchRequest::with_params(context, query, params))?
                }
                None => serialize_body(&SearchRequest::new(context, query))?,
            },
        };

        let started = Instant::now();
        let (payload, _) = self.client.post_json("search", body, ctx).await?;
        let elapsed = started.elapsed();
        tracing::debug!(
            provider = %self.config.provider_id,
            endpoint = "search",
            latency_ms = elapsed.as_millis() as u64,
            "innertube search"
        );

        let mut report = ParseReport::new();
        let renderers = collect_renderers(&payload, &mut report);
        if renderers.is_empty() {
            // A renderer-less document is not "a page with nothing on it" — it is a shape we do not
            // recognise. Recording it as structural means `parse_warnings` reflects it and strict
            // mode refuses it, instead of handing back an empty page that looks like a valid answer.
            report.structural_problem("no_renderers");
        }
        let provenance = self.provenance("search", &report, OffsetDateTime::now_utc());
        let mut results = map_search(&payload, renderers, &mut report, provenance);
        results.provenance.parse_warnings = report.count();

        // Respect the caller's page size without pretending the upstream honoured it.
        if results.items.len() > usize::from(limit) {
            results.items.truncate(usize::from(limit));
            report.warn("page_truncated");
            results.provenance.parse_warnings = report.count();
        }

        report.check(ctx.strictness)?;
        Ok(results)
    }

    async fn video(
        &self,
        id: vydra_common::VideoId,
        ctx: &ProviderContext,
    ) -> Result<VideoDocument, ProviderError> {
        let session = self.client.session(ctx).await;
        let context = self.client.context(&session, ctx);
        let body = serialize_body(&PlayerRequest::new(context, id.as_str()))?;

        let (payload, _) = self.client.post_json("player", body, ctx).await?;

        let mut report = ParseReport::new();
        let provenance = self.provenance("player", &report, OffsetDateTime::now_utc());
        let mut document = map_player(&payload, &id, &mut report, provenance)?;
        document.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(document)
    }

    /// Fetch a browse document and build the parse report.
    ///
    /// The report is returned with the payload because both map failures (`Err`) and provenance need it;
    /// `post_json` errors (transport, status) propagate untouched so their fallback semantics are kept.
    async fn browse(
        &self,
        body: BrowseRequest,
        ctx: &ProviderContext,
    ) -> Result<(serde_json::Value, ParseReport), ProviderError> {
        let encoded = serialize_body(&body)?;
        let started = Instant::now();
        let (payload, _) = self.client.post_json("browse", encoded, ctx).await?;
        tracing::debug!(
            provider = %self.config.provider_id,
            endpoint = "browse",
            latency_ms = started.elapsed().as_millis() as u64,
            "innertube browse"
        );
        Ok((payload, ParseReport::new()))
    }

    async fn channel(
        &self,
        reference: ChannelRef,
        ctx: &ProviderContext,
    ) -> Result<ChannelDocument, ProviderError> {
        if !self.capabilities.contains(Capability::ChannelMetadata) {
            return Err(ProviderError::Unsupported {
                capability: Capability::ChannelMetadata.as_str().to_owned(),
            });
        }
        // A `@handle` is not a browse id: the Phase 6 probe sent one and upstream answered `400`
        // (`docs/research/CHANNEL_CONTRACT.md` §7). Resolving a handle would need a second request whose
        // result is a guess about which channel the caller meant, so the request is refused instead of
        // being translated into a search.
        if matches!(reference, ChannelRef::Handle(_)) {
            return Err(ProviderError::Unsupported {
                capability: "channel_handle".to_owned(),
            });
        }
        let session = self.client.session(ctx).await;
        let context = self.client.context(&session, ctx);
        let body = BrowseRequest::channel(context, &reference);
        let (payload, mut report) = self.browse(body, ctx).await?;
        let provenance = self.provenance("browse", &report, OffsetDateTime::now_utc());
        let mut document = map_channel(&payload, &reference, &mut report, provenance)?;
        document.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(document)
    }

    /// One page of a channel's video list.
    ///
    /// The **first** page costs two browse calls: the tab `params` blob is relayed from the channel page
    /// instead of being hard-coded (`EgZ2aWRlb3PyBgQKAjoA` in the reference extractors is exactly the
    /// kind of unverified constant this phase refuses to ship). Continuation pages cost one call,
    /// because the token already encodes the tab and the offset.
    async fn channel_videos(
        &self,
        id: ChannelId,
        cursor: Option<String>,
        limit: u16,
        ctx: &ProviderContext,
    ) -> Result<ChannelVideoList, ProviderError> {
        if !self.capabilities.contains(Capability::ChannelVideos) {
            return Err(ProviderError::Unsupported {
                capability: Capability::ChannelVideos.as_str().to_owned(),
            });
        }
        let session = self.client.session(ctx).await;

        let (payload, mut report) = match cursor {
            Some(token) => {
                let context = self.client.context(&session, ctx);
                self.browse_continuation(context, token, ctx).await?
            }
            None => {
                let page_context = self.client.context(&session, ctx);
                let (page, mut page_report) = self
                    .browse(
                        BrowseRequest::channel(page_context, &ChannelRef::Id(id.clone())),
                        ctx,
                    )
                    .await?;
                let Some(params) = super::browse_map::videos_tab_params(&page) else {
                    // No videos tab: a channel can legitimately have none (an auto-generated channel, or
                    // one whose only content is Shorts). That is an unsupported resource, not a 500.
                    page_report.warn("channel_videos_tab_missing");
                    return Err(ProviderError::Unsupported {
                        capability: "channel_videos_tab".to_owned(),
                    });
                };
                let context = self.client.context(&session, ctx);
                let body = BrowseRequest::channel_tab(context, &id, params)?;
                self.browse(body, ctx).await?
            }
        };

        let provenance = self.provenance("browse", &report, OffsetDateTime::now_utc());
        let mut list = map_channel_videos(&payload, limit, &mut report, provenance);
        list.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(list)
    }

    async fn playlist(
        &self,
        id: PlaylistId,
        ctx: &ProviderContext,
    ) -> Result<PlaylistDocument, ProviderError> {
        if !self.capabilities.contains(Capability::PlaylistMetadata) {
            return Err(ProviderError::Unsupported {
                capability: Capability::PlaylistMetadata.as_str().to_owned(),
            });
        }
        let session = self.client.session(ctx).await;
        let context = self.client.context(&session, ctx);
        let body = BrowseRequest::playlist(context, &id);
        let (payload, mut report) = self.browse(body, ctx).await?;
        let provenance = self.provenance("browse", &report, OffsetDateTime::now_utc());
        let mut document = map_playlist(&payload, &id, PAGE_ROWS, &mut report, provenance)?;
        document.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(document)
    }

    /// One page of a video's comments.
    ///
    /// Request count, stated because it is not one:
    ///
    /// * page 1, default order — two calls: the watch page (to find the comment section's continuation
    ///   token) and the comment section itself;
    /// * page 1, `newest` — three calls when the page opens in another order, because the token for the
    ///   requested order exists only in the sort menu of a comment page;
    /// * page *n* — one call.
    ///
    /// The engine caches page 1, which is what keeps the two-call discovery from being paid per request.
    async fn comments(
        &self,
        id: VideoId,
        sort: CommentSort,
        cursor: Option<String>,
        limit: u16,
        ctx: &ProviderContext,
    ) -> Result<CommentList, ProviderError> {
        if !self.capabilities.contains(Capability::CommentsThread) {
            return Err(ProviderError::Unsupported {
                capability: Capability::CommentsThread.as_str().to_owned(),
            });
        }
        let session = self.client.session(ctx).await;

        let (payload, sort, mut report) = match cursor {
            Some(token) => {
                let context = self.client.context(&session, ctx);
                let (payload, report) = self.next_page(context, token, ctx).await?;
                (payload, sort, report)
            }
            None => {
                let token = self.comments_section_token(&session, &id, ctx).await?;
                let (first, mut report) = self.comments_page(&session, token, ctx).await?;
                let offered = comments::sort_options(&first);
                let selected = offered
                    .iter()
                    .position(|option| option.selected)
                    .unwrap_or(0);
                let target = match sort {
                    CommentSort::Top => 0,
                    CommentSort::Newest => 1,
                };
                if selected == target {
                    (first, sort, report)
                } else {
                    let token = comments::sort_token(&offered, sort)?;
                    let (page, page_report) = self.comments_page(&session, token, ctx).await?;
                    report.merge(page_report);
                    (page, sort, report)
                }
            }
        };

        let provenance = self.provenance("next", &report, OffsetDateTime::now_utc());
        let mut list = comments::map_comments_page(&payload, sort, limit, &mut report, provenance);
        list.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(list)
    }

    /// One page of replies under a comment.
    ///
    /// A reply continuation token exists only inside the comment page that rendered the thread, so page 1
    /// has to find the thread first: Vydra scans the default comment pages (bounded, see
    /// [`REPLY_SCAN_PAGES`]) and reports `not_found` when the thread is not among them. It never builds a
    /// token for a comment id, because that encoding is not documented and a wrong token would page some
    /// other thread.
    async fn comment_replies(
        &self,
        id: VideoId,
        parent: CommentId,
        cursor: Option<String>,
        limit: u16,
        ctx: &ProviderContext,
    ) -> Result<ReplyList, ProviderError> {
        if !self.capabilities.contains(Capability::CommentsReplies) {
            return Err(ProviderError::Unsupported {
                capability: Capability::CommentsReplies.as_str().to_owned(),
            });
        }
        let session = self.client.session(ctx).await;

        let (payload, mut report) = match cursor {
            Some(token) => {
                let context = self.client.context(&session, ctx);
                self.next_page(context, token, ctx).await?
            }
            None => {
                let token = self
                    .find_reply_token(&session, &id, &parent, ctx, &mut ParseReport::new())
                    .await?;
                let context = self.client.context(&session, ctx);
                self.next_page(context, token, ctx).await?
            }
        };

        let provenance = self.provenance("next", &report, OffsetDateTime::now_utc());
        let mut page = comments::map_replies_page(&payload, &parent, limit, &mut report, provenance);
        page.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(page)
    }

    /// The comment section's continuation token, from the video's watch page.
    ///
    /// A watch page with no comment section is how upstream says "comments are turned off": the section is
    /// absent, not empty. That is a content fact (`Unavailable{CommentsDisabled}`), not a parse failure, and
    /// it is what the API reports as `409 comments_disabled` rather than an empty list.
    async fn comments_section_token(
        &self,
        session: &super::client::SessionData,
        id: &VideoId,
        ctx: &ProviderContext,
    ) -> Result<String, ProviderError> {
        let context = self.client.context(session, ctx);
        let body = serialize_body(&NextRequest::comments_section(context, id))?;
        let (payload, _) = self.client.post_json("next", body, ctx).await?;
        match comments::comments_section_token(&payload)? {
            Some(token) => Ok(token),
            None => Err(ProviderError::Unavailable {
                reason: vydra_domain::UnavailabilityReason::CommentsDisabled,
                message: "watch page has no comment section".to_owned(),
            }),
        }
    }

    /// One `next` continuation page.
    async fn next_page(
        &self,
        context: super::request::ClientContext,
        token: String,
        ctx: &ProviderContext,
    ) -> Result<(Value, ParseReport), ProviderError> {
        let body = serialize_body(&NextRequest::continuation(context, token)?)?;
        let started = Instant::now();
        let (payload, _) = self.client.post_json("next", body, ctx).await?;
        tracing::debug!(
            provider = %self.config.provider_id,
            endpoint = "next",
            continuation = true,
            latency_ms = started.elapsed().as_millis() as u64,
            "innertube next continuation"
        );
        Ok((payload, ParseReport::new()))
    }

    /// One comment page addressed by a relayed token.
    async fn comments_page(
        &self,
        session: &super::client::SessionData,
        token: String,
        ctx: &ProviderContext,
    ) -> Result<(Value, ParseReport), ProviderError> {
        let context = self.client.context(session, ctx);
        self.next_page(context, token, ctx).await
    }

    /// Find the reply continuation token for `parent` by scanning the default comment pages.
    ///
    /// Bounded on purpose: every scanned page is a real upstream request, so the constant is a cost limit,
    /// not a correctness knob. A thread that is not among the scanned pages is `NotFound` with a message
    /// that says so, which is the honest answer — the alternative would be a token Vydra cannot verify.
    async fn find_reply_token(
        &self,
        session: &super::client::SessionData,
        id: &VideoId,
        parent: &CommentId,
        ctx: &ProviderContext,
        report: &mut ParseReport,
    ) -> Result<String, ProviderError> {
        let mut token = self.comments_section_token(session, id, ctx).await?;
        for page in 0..REPLY_SCAN_PAGES {
            let (payload, page_report) = self.comments_page(session, token, ctx).await?;
            report.merge(page_report);
            if let Some(token) = comments::replies_token_for(&payload, parent) {
                return Ok(token);
            }
            let Some(next) = comments::next_page_token(&payload) else {
                break;
            };
            if page + 1 < REPLY_SCAN_PAGES {
                token = next;
            }
        }
        Err(ProviderError::NotFound {
            id: parent.as_str().to_owned(),
            message: format!(
                "comment thread not found on the first {REPLY_SCAN_PAGES} comment pages of {id}"
            ),
        })
    }

    async fn playlist_videos(
        &self,
        id: PlaylistId,
        cursor: Option<String>,
        limit: u16,
        ctx: &ProviderContext,
    ) -> Result<PlaylistVideoList, ProviderError> {
        if !self.capabilities.contains(Capability::PlaylistItems) {
            return Err(ProviderError::Unsupported {
                capability: Capability::PlaylistItems.as_str().to_owned(),
            });
        }
        let session = self.client.session(ctx).await;
        let (payload, mut report) = match cursor {
            Some(token) => {
                let context = self.client.context(&session, ctx);
                self.browse_continuation(context, token, ctx).await?
            }
            None => {
                let context = self.client.context(&session, ctx);
                self.browse(BrowseRequest::playlist(context, &id), ctx)
                    .await?
            }
        };
        let provenance = self.provenance("browse", &report, OffsetDateTime::now_utc());
        let mut list = map_playlist_videos(&payload, limit, &mut report, provenance);
        list.provenance.parse_warnings = report.count();
        report.check(ctx.strictness)?;
        Ok(list)
    }

    /// Continuation body for a browse page: `{context, continuation}` and nothing else.
    async fn browse_continuation(
        &self,
        context: super::request::ClientContext,
        token: String,
        ctx: &ProviderContext,
    ) -> Result<(serde_json::Value, ParseReport), ProviderError> {
        let encoded = serialize_body(&ContinuationRequest::new(context, token))?;
        let started = Instant::now();
        let (payload, _) = self.client.post_json("browse", encoded, ctx).await?;
        tracing::debug!(
            provider = %self.config.provider_id,
            endpoint = "browse",
            continuation = true,
            latency_ms = started.elapsed().as_millis() as u64,
            "innertube browse continuation"
        );
        Ok((payload, ParseReport::new()))
    }
}

#[async_trait::async_trait]
impl Provider for InnertubeProvider {
    fn id(&self) -> ProviderId {
        self.config.provider_id
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    fn client_label(&self) -> String {
        self.client.profile().label()
    }

    async fn fetch(
        &self,
        request: ProviderRequest,
        ctx: &ProviderContext,
    ) -> Result<ProviderResponse, ProviderError> {
        if ctx.is_expired() {
            return Err(ProviderError::Timeout);
        }
        match request {
            ProviderRequest::Search {
                query,
                kind,
                filters,
                cursor,
                limit,
                ..
            } => {
                if query.trim().is_empty() {
                    return Err(ProviderError::InvalidRequest {
                        field: "q".into(),
                        issue: "empty".into(),
                    });
                }
                self.search(query, kind, filters, cursor, ctx, limit)
                    .await
                    .map(ProviderResponse::Search)
            }
            ProviderRequest::Video { id, .. } => self
                .video(id, ctx)
                .await
                .map(|doc| ProviderResponse::Video(Box::new(doc))),
            ProviderRequest::CaptionTracks { id, .. } => {
                let document = self.video(id, ctx).await?;
                Ok(ProviderResponse::CaptionTracks(CaptionTrackList {
                    items: document.caption_tracks,
                    provenance: document.provenance,
                }))
            }
            ProviderRequest::Channel { reference, .. } => self
                .channel(reference, ctx)
                .await
                .map(|document| ProviderResponse::Channel(Box::new(document))),
            ProviderRequest::ChannelVideos {
                id, cursor, limit, ..
            } => self
                .channel_videos(id, cursor, limit, ctx)
                .await
                .map(ProviderResponse::ChannelVideos),
            ProviderRequest::Playlist { id, .. } => self
                .playlist(id, ctx)
                .await
                .map(|document| ProviderResponse::Playlist(Box::new(document))),
            ProviderRequest::PlaylistVideos {
                id, cursor, limit, ..
            } => self
                .playlist_videos(id, cursor, limit, ctx)
                .await
                .map(ProviderResponse::PlaylistVideos),
            ProviderRequest::Comments {
                id,
                sort,
                cursor,
                limit,
                ..
            } => self
                .comments(id, sort, cursor, limit, ctx)
                .await
                .map(ProviderResponse::Comments),
            ProviderRequest::CommentReplies {
                id,
                parent,
                cursor,
                limit,
                ..
            } => self
                .comment_replies(id, parent, cursor, limit, ctx)
                .await
                .map(ProviderResponse::CommentReplies),
            ProviderRequest::Unsupported { resource, .. } => Err(ProviderError::Unsupported {
                capability: resource.to_owned(),
            }),
        }
    }
}

/// Serialize a typed request body.
///
/// The bodies are plain structs, so this cannot fail in practice; the error path exists so that a
/// future field type with a fallible `Serialize` cannot panic in a provider.
fn serialize_body<T: Serialize>(body: &T) -> Result<serde_json::Value, ProviderError> {
    serde_json::to_value(body).map_err(|err| ProviderError::InvalidRequest {
        field: "body".to_owned(),
        issue: format!("serialize: {err}"),
    })
}
