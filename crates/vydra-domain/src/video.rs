//! Video documents.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use vydra_common::{ChannelId, VideoId};

use crate::common::{
    Availability, CaptionTrack, Chapter, Count, LiveState, MediaFormatInfo, Provenance, Thumbnail,
};

/// Compact video representation used by search results, playlists, feeds and related lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSummary {
    pub id: VideoId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_title: Option<String>,
    /// Duration in whole seconds. `None` for live/premiere items and when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<Count>,
    /// Verbatim localized relative date (`"3 days ago"`), kept because it is locale-dependent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_text: Option<String>,
    /// Absolute publication time when the provider exposes one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub published_at: Option<OffsetDateTime>,
    /// Ordered smallest → largest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
    /// Broadcast state when the provider states it; `None` means "not reported".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_state: Option<LiveState>,
    /// Badge labels as rendered (`"4K"`, `"CC"`, `"New"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
}

impl VideoSummary {
    /// Domain minimum for a usable summary (used by provider self-checks).
    pub fn is_complete(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

/// Full video document served by `GET /videos/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoDocument {
    pub summary: VideoSummary,
    pub availability: Availability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count: Option<Count>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<Count>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_tracks: Vec<CaptionTrack>,
    /// Format metadata only — never media URLs (ADR-006).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<MediaFormatInfo>,
    pub provenance: Provenance,
}

/// Document returned by `GET /videos/{id}/related`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedVideos {
    pub items: Vec<VideoSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub provenance: Provenance,
}

/// Document returned by `GET /videos/{id}/chapters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoChapters {
    pub items: Vec<Chapter>,
    pub provenance: Provenance,
}
