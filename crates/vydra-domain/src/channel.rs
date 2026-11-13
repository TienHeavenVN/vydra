//! Channel documents.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use vydra_common::ChannelId;

use crate::common::{Availability, Count, Provenance, Thumbnail};
use crate::video::VideoSummary;

/// Compact channel representation used inside search results and video documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSummary {
    pub id: ChannelId,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscriber_count: Option<Count>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_count: Option<Count>,
}

/// A link on the channel page ("about" tab, external sites, social profiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelLink {
    pub title: String,
    pub url: String,
}

/// A tab exposed by the channel page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelTab {
    pub title: String,
    /// Stable slug used in the API (`videos`, `playlists`, `shorts`, `live`), when recognized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

/// Document returned by `GET /channels/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDocument {
    pub summary: ChannelSummary,
    pub availability: Availability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<Thumbnail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<ChannelLink>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub joined_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_view_count: Option<Count>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<ChannelTab>,
    pub provenance: Provenance,
}

/// Paged list of a channel's videos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelVideoList {
    pub items: Vec<VideoSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub provenance: Provenance,
}
