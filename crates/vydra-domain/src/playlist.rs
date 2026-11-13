//! Playlist documents.

use serde::{Deserialize, Serialize};
use vydra_common::{ChannelId, PlaylistId};

use crate::common::{Availability, Count, Provenance, Thumbnail};
use crate::video::VideoSummary;

/// Compact playlist representation used inside search results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: PlaylistId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<Count>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
}

/// One entry in a playlist, preserving upstream ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// 1-based position when the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    pub video: VideoSummary,
}

/// Document returned by `GET /playlists/{id}` and `GET /playlists/{id}/items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistDocument {
    pub summary: PlaylistSummary,
    pub availability: Availability,
    pub items: Vec<PlaylistItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub provenance: Provenance,
}
