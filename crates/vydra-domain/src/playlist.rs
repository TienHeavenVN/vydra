//! Playlist documents.
//!
//! Normative contract: `docs/research/PLAYLIST_CONTRACT.md`.
//!
//! The hard part of a playlist is not the header, it is the entry list: a playlist is an *ordered*
//! collection, and its owner can delete, privatize, region-block or reorder any entry at any time.
//! Upstream keeps the row and flips a playability flag, so a naive parser that skips unplayable rows
//! silently turns "position 7 is gone" into "what was at 8 is now at 7". Every entry therefore keeps its
//! position, its validation verdict and its availability, even when it carries no playable video.

use serde::{Deserialize, Serialize};
use vydra_common::{ChannelId, PlaylistId, VideoId};

use crate::common::{Availability, Count, MetadataEvidence, Provenance, Thumbnail};

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

/// Which upstream renderer produced a playlist entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistEntrySource {
    /// `playlistVideoRenderer` — the classic row, carries `isPlayable` and `index`.
    PlaylistVideo,
    /// `lockupViewModel` — the newer row; it has no `isPlayable` field at all.
    Lockup,
    /// A renderer Vydra recognizes as a playlist row but cannot read every field from.
    Other,
}

/// One entry in a playlist, preserving upstream ordering.
///
/// `position` is `None` when the row did not state one. It is **never** computed from the offset in the
/// page: a continuation page starts at an arbitrary offset, and a computed position would disagree with
/// the number the owner sees in the YouTube UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// 1-based position exactly as the row reported it (`index`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    /// Video id, present only when it passed [`VideoId`] validation.
    ///
    /// An unparseable or absent id is `None` with an `Unavailable`/`Unknown` availability — a lookalike
    /// string never becomes a video reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_id: Option<VideoId>,
    /// Title as rendered: for a deleted entry this is often `[Deleted video]`, and it is kept verbatim.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
    /// Per-entry availability. `Unknown` means the row did not state playability (the `lockupViewModel`
    /// shape has no `isPlayable`), which is not the same thing as "available".
    pub availability: Availability,
    pub source: PlaylistEntrySource,
}

impl PlaylistItem {
    /// Whether this entry can be handed to a client as a playable video reference.
    pub fn is_playable(&self) -> bool {
        self.video_id.is_some() && self.availability.is_available()
    }
}

/// Document returned by `GET /api/v1/playlists/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistDocument {
    pub summary: PlaylistSummary,
    pub availability: Availability,
    /// First page of entries, in upstream order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<PlaylistItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Views as stated on the playlist page, when it states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<Count>,
    /// The page's own wording for "last updated" (e.g. `Last updated on 3 Jun 2024`), untranslated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated_text: Option<String>,
    /// How many entries repeated a video id already seen earlier in *this* page.
    ///
    /// Nothing is dropped: a playlist may legitimately contain the same video twice, so removing the
    /// repeat would shorten the list and shift every later position. The count exists so that a client
    /// (and a test) can tell "the owner added it twice" from "the parser emitted it twice".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeated_entries: Option<u32>,
    #[serde(default, skip_serializing_if = "MetadataEvidence::is_empty")]
    pub evidence: MetadataEvidence,
    pub provenance: Provenance,
}

impl PlaylistDocument {
    pub fn id(&self) -> &PlaylistId {
        &self.summary.id
    }
}

/// Paged entry list returned by `GET /api/v1/playlists/{id}/videos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistVideoList {
    pub items: Vec<PlaylistItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many entries repeated a video id already seen earlier in *this* page (see
    /// [`PlaylistDocument::repeated_entries`]); nothing is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeated_entries: Option<u32>,
    pub provenance: Provenance,
}
