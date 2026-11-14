//! Search results.

use serde::{Deserialize, Serialize};
use vydra_common::PlaylistId;

use crate::channel::ChannelSummary;
use crate::common::{Count, Provenance};
use crate::playlist::PlaylistSummary;
use crate::video::VideoSummary;

/// One search hit. Mixed search returns a heterogeneous list, so the kind is explicit in JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchItem {
    Video(VideoSummary),
    Channel(ChannelSummary),
    Playlist(PlaylistSummary),
}

impl SearchItem {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Video(_) => "video",
            Self::Channel(_) => "channel",
            Self::Playlist(_) => "playlist",
        }
    }
}

/// Result of `GET /search*`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    pub items: Vec<SearchItem>,
    /// Signed cursor for the next page; `None` when exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Upstream-reported result estimate, when exposed (approximate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_results: Option<Count>,
    pub provenance: Provenance,
}

impl SearchResults {
    pub fn video_ids(&self) -> Vec<&vydra_common::VideoId> {
        self.items
            .iter()
            .filter_map(|item| match item {
                SearchItem::Video(video) => Some(&video.id),
                _ => None,
            })
            .collect()
    }

    pub fn playlist_ids(&self) -> Vec<&PlaylistId> {
        self.items
            .iter()
            .filter_map(|item| match item {
                SearchItem::Playlist(playlist) => Some(&playlist.id),
                _ => None,
            })
            .collect()
    }
}
