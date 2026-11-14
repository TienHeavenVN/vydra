//! The provider contract.
//!
//! Normative documentation: `docs/architecture/PROVIDER_INTERFACE.md`.
//!
//! Design summary:
//! - a provider declares [`Capability`]s and can only be routed requests it declared;
//! - it returns [`vydra_domain`] types (never provider JSON) or a typed [`ProviderError`];
//! - it never performs HTTP directly: everything goes through [`Transport`] so that parser tests are
//!   deterministic and SSRF/timeout/size limits cannot be bypassed.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod innertube;
pub mod parse;
pub mod transport;

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use vydra_common::{ChannelId, ChannelRef, PlaylistId, VideoId};
use vydra_domain::{
    CaptionTrackList, ChannelDocument, ChannelVideoList, PlaylistDocument, PlaylistVideoList,
    SearchResults, VideoDocument,
};
use vydra_errors::ProviderError;

pub use transport::{
    HttpMethod, ReplayTransport, ReqwestTransport, Transport, TransportError, TransportRequest,
    TransportResponse,
};

/// Stable provider identifier, e.g. `innertube-web`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(&'static str);

impl ProviderId {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A capability a provider may declare. Bit indices are stable and part of the wire contract for
/// `GET /status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Capability {
    SearchAll = 0,
    SearchVideos = 1,
    SearchChannels = 2,
    SearchPlaylists = 3,
    SearchFilters = 4,
    SearchSort = 5,
    SearchSuggestions = 6,
    SearchMusic = 7,
    VideoMetadata = 8,
    VideoRelated = 9,
    VideoChapters = 10,
    VideoFormatsMetadata = 11,
    VideoSubtitlesMetadata = 12,
    ChannelMetadata = 13,
    ChannelVideos = 14,
    ChannelPlaylists = 15,
    ChannelAbout = 16,
    PlaylistMetadata = 17,
    PlaylistItems = 18,
    CommentsThread = 19,
    CommentsReplies = 20,
    CaptionsMetadata = 21,
    CaptionsTranscript = 22,
    LiveMetadata = 23,
    LiveChatMetadata = 24,
    Trending = 25,
    Categories = 26,
    Recommendations = 27,
    CursorPagination = 28,
    LocaleSensitive = 29,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Self::SearchAll,
        Self::SearchVideos,
        Self::SearchChannels,
        Self::SearchPlaylists,
        Self::SearchFilters,
        Self::SearchSort,
        Self::SearchSuggestions,
        Self::SearchMusic,
        Self::VideoMetadata,
        Self::VideoRelated,
        Self::VideoChapters,
        Self::VideoFormatsMetadata,
        Self::VideoSubtitlesMetadata,
        Self::ChannelMetadata,
        Self::ChannelVideos,
        Self::ChannelPlaylists,
        Self::ChannelAbout,
        Self::PlaylistMetadata,
        Self::PlaylistItems,
        Self::CommentsThread,
        Self::CommentsReplies,
        Self::CaptionsMetadata,
        Self::CaptionsTranscript,
        Self::LiveMetadata,
        Self::LiveChatMetadata,
        Self::Trending,
        Self::Categories,
        Self::Recommendations,
        Self::CursorPagination,
        Self::LocaleSensitive,
    ];

    pub const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SearchAll => "search_all",
            Self::SearchVideos => "search_videos",
            Self::SearchChannels => "search_channels",
            Self::SearchPlaylists => "search_playlists",
            Self::SearchFilters => "search_filters",
            Self::SearchSort => "search_sort",
            Self::SearchSuggestions => "search_suggestions",
            Self::SearchMusic => "search_music",
            Self::VideoMetadata => "video_metadata",
            Self::VideoRelated => "video_related",
            Self::VideoChapters => "video_chapters",
            Self::VideoFormatsMetadata => "video_formats_metadata",
            Self::VideoSubtitlesMetadata => "video_subtitles_metadata",
            Self::ChannelMetadata => "channel_metadata",
            Self::ChannelVideos => "channel_videos",
            Self::ChannelPlaylists => "channel_playlists",
            Self::ChannelAbout => "channel_about",
            Self::PlaylistMetadata => "playlist_metadata",
            Self::PlaylistItems => "playlist_items",
            Self::CommentsThread => "comments_thread",
            Self::CommentsReplies => "comments_replies",
            Self::CaptionsMetadata => "captions_metadata",
            Self::CaptionsTranscript => "captions_transcript",
            Self::LiveMetadata => "live_metadata",
            Self::LiveChatMetadata => "live_chat_metadata",
            Self::Trending => "trending",
            Self::Categories => "categories",
            Self::Recommendations => "recommendations",
            Self::CursorPagination => "cursor_pagination",
            Self::LocaleSensitive => "locale_sensitive",
        }
    }
}

/// Set of capabilities. Cheap to copy, `const`-constructible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const NONE: Self = Self(0);

    pub const fn from_slice(caps: &[Capability]) -> Self {
        let mut bits = 0u64;
        let mut i = 0;
        while i < caps.len() {
            bits |= caps[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub const fn with(self, cap: Capability) -> Self {
        Self(self.0 | cap.bit())
    }

    pub const fn contains(self, cap: Capability) -> bool {
        self.0 & cap.bit() != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(move |cap| self.contains(*cap))
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for cap in self.iter() {
            if !first {
                f.write_str(",")?;
            }
            f.write_str(cap.as_str())?;
            first = false;
        }
        Ok(())
    }
}

/// Which search surface is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Videos,
    Channels,
    Playlists,
    Music,
}

impl SearchKind {
    pub const fn required_capability(self) -> Capability {
        match self {
            Self::All => Capability::SearchAll,
            Self::Videos => Capability::SearchVideos,
            Self::Channels => Capability::SearchChannels,
            Self::Playlists => Capability::SearchPlaylists,
            Self::Music => Capability::SearchMusic,
        }
    }

    pub const fn cursor_namespace(self) -> &'static str {
        match self {
            Self::All => "search.all",
            Self::Videos => "search.videos",
            Self::Channels => "search.channels",
            Self::Playlists => "search.playlists",
            Self::Music => "search.music",
        }
    }
}

/// Sort order for search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchSort {
    #[default]
    Relevance,
    UploadDate,
    ViewCount,
    Rating,
}

/// Upload-date filter for search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDate {
    Hour,
    Day,
    Week,
    Month,
    Year,
}

/// Duration filter for search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDuration {
    Short,
    Medium,
    Long,
}

/// Feature filter for search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFeature {
    Live,
    Hd,
    Uhd4k,
    Subtitles,
    CreativeCommons,
    ThreeD,
    ThreeSixty,
    Vr180,
    Hdr,
}

/// Normalized search filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub sort: SearchSort,
    pub uploaded: Option<SearchDate>,
    pub duration: Option<SearchDuration>,
    pub features: Vec<SearchFeature>,
}

/// Typed provider request. Adding a variant is an API change; adding a provider is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRequest {
    Search {
        query: String,
        kind: SearchKind,
        filters: SearchFilters,
        /// Opaque cursor previously minted by the engine for this kind.
        cursor: Option<String>,
        hl: String,
        gl: String,
        limit: u16,
    },
    Video {
        id: VideoId,
        hl: String,
        gl: String,
    },
    CaptionTracks {
        id: VideoId,
        hl: String,
        gl: String,
    },
    /// Channel metadata page (`GET /api/v1/channels/{id}`).
    ///
    /// The reference may be a validated `UC…` id **or** a `@handle`. A handle is not a channel id and is
    /// never converted into one here: it is sent as the browse id upstream uses for the vanity URL, and
    /// the response's canonical id is checked against it (`browse_map::map_channel`).
    Channel {
        reference: ChannelRef,
        hl: String,
        gl: String,
    },
    /// One page of a channel's video list. `cursor` is a provider continuation token, never a
    /// screen-reader "sort by" chip (see `parse::find_page_cursor`).
    ChannelVideos {
        id: ChannelId,
        cursor: Option<String>,
        hl: String,
        gl: String,
        limit: u16,
    },
    /// Playlist metadata plus the first page of entries.
    Playlist {
        id: PlaylistId,
        hl: String,
        gl: String,
    },
    /// One page of a playlist's entries.
    PlaylistVideos {
        id: PlaylistId,
        cursor: Option<String>,
        hl: String,
        gl: String,
        limit: u16,
    },
    /// Placeholder for resources whose providers are not implemented yet. The registry refuses to
    /// route these (no capability declared), so an endpoint is never mounted on a stub.
    Unsupported {
        resource: &'static str,
        channel: Option<ChannelRef>,
        playlist: Option<PlaylistId>,
    },
}

impl ProviderRequest {
    /// Capability required to serve this request.
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::Search { kind, .. } => kind.required_capability(),
            Self::Video { .. } => Capability::VideoMetadata,
            Self::CaptionTracks { .. } => Capability::CaptionsMetadata,
            Self::Channel { .. } => Capability::ChannelMetadata,
            Self::ChannelVideos { .. } => Capability::ChannelVideos,
            Self::Playlist { .. } => Capability::PlaylistMetadata,
            Self::PlaylistVideos { .. } => Capability::PlaylistItems,
            Self::Unsupported { .. } => Capability::SearchAll,
        }
    }

    /// Endpoint family, used for provenance and cache keys.
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::Search { .. } => "search",
            Self::Video { .. } => "player",
            Self::CaptionTracks { .. } => "player",
            // Channels and playlists are both browse calls; the sub-resource is in the cache key.
            Self::Channel { .. } | Self::ChannelVideos { .. } => "browse",
            Self::Playlist { .. } | Self::PlaylistVideos { .. } => "browse",
            Self::Unsupported { .. } => "unsupported",
        }
    }

    pub fn locale(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Search { hl, gl, .. }
            | Self::Video { hl, gl, .. }
            | Self::CaptionTracks { hl, gl, .. }
            | Self::Channel { hl, gl, .. }
            | Self::ChannelVideos { hl, gl, .. }
            | Self::Playlist { hl, gl, .. }
            | Self::PlaylistVideos { hl, gl, .. } => (Some(hl), Some(gl)),
            Self::Unsupported { .. } => (None, None),
        }
    }
}

/// Typed provider response. Only domain types cross this boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResponse {
    Search(SearchResults),
    Video(Box<VideoDocument>),
    CaptionTracks(CaptionTrackList),
    Channel(Box<ChannelDocument>),
    ChannelVideos(ChannelVideoList),
    Playlist(Box<PlaylistDocument>),
    PlaylistVideos(PlaylistVideoList),
}

impl ProviderResponse {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Self::Search(_) => "search",
            Self::Video(_) => "player",
            Self::CaptionTracks(_) => "player",
            Self::Channel(_) | Self::ChannelVideos(_) => "browse",
            Self::Playlist(_) | Self::PlaylistVideos(_) => "browse",
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Search(results) => results.items.is_empty(),
            Self::Video(_) => false,
            Self::CaptionTracks(tracks) => tracks.items.is_empty(),
            // A channel or playlist document is content even when it carries no entries: the header is
            // the payload. Listing pages, by contrast, are empty when they have no items.
            Self::Channel(_) | Self::Playlist(_) => false,
            Self::ChannelVideos(list) => list.items.is_empty(),
            Self::PlaylistVideos(list) => list.items.is_empty(),
        }
    }
}

/// Parser strictness. `Strict` turns any parse warning into an error (used by tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    Lenient,
    Strict,
}

/// Per-call context.
#[derive(Clone)]
pub struct ProviderContext {
    /// Absolute deadline; providers must not run past it.
    pub deadline: Instant,
    pub request_id: String,
    /// Defaults already normalized by the engine.
    pub hl: String,
    pub gl: String,
    pub strictness: Strictness,
}

impl fmt::Debug for ProviderContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderContext")
            .field("request_id", &self.request_id)
            .field("hl", &self.hl)
            .field("gl", &self.gl)
            .field("strictness", &self.strictness)
            .finish_non_exhaustive()
    }
}

impl ProviderContext {
    pub fn remaining(&self) -> std::time::Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn is_expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

/// The provider contract.
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    fn id(&self) -> ProviderId;

    fn capabilities(&self) -> CapabilitySet;

    /// Human-readable description of the upstream client/profile in use (provenance).
    fn client_label(&self) -> String;

    async fn fetch(
        &self,
        request: ProviderRequest,
        ctx: &ProviderContext,
    ) -> Result<ProviderResponse, ProviderError>;
}

/// Convenience alias for a shared provider.
pub type SharedProvider = Arc<dyn Provider>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_set_contains_declared_bits_only() {
        let set = CapabilitySet::from_slice(&[Capability::SearchVideos, Capability::VideoMetadata]);
        assert!(set.contains(Capability::SearchVideos));
        assert!(set.contains(Capability::VideoMetadata));
        assert!(!set.contains(Capability::CommentsThread));
        assert_eq!(set.iter().count(), 2);
    }

    #[test]
    fn capability_union_is_additive() {
        let a = CapabilitySet::from_slice(&[Capability::SearchAll]);
        let b = CapabilitySet::from_slice(&[Capability::VideoMetadata]);
        let both = a.union(b);
        assert_eq!(both.to_string(), "search_all,video_metadata");
    }

    #[test]
    fn request_capabilities_are_derived_from_kind() {
        let request = ProviderRequest::Search {
            query: "x".into(),
            kind: SearchKind::Playlists,
            filters: SearchFilters::default(),
            cursor: None,
            hl: "en".into(),
            gl: "US".into(),
            limit: 20,
        };
        assert_eq!(request.required_capability(), Capability::SearchPlaylists);
        assert_eq!(request.endpoint(), "search");
    }

    #[test]
    fn cursor_namespace_is_stable_per_kind() {
        assert_eq!(SearchKind::Videos.cursor_namespace(), "search.videos");
        assert_eq!(SearchKind::Channels.cursor_namespace(), "search.channels");
    }
}
