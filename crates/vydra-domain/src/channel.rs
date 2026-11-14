//! Channel documents.
//!
//! Normative contract: `docs/research/CHANNEL_CONTRACT.md`.
//!
//! Two rules shape this module:
//!
//! 1. **A missing field is not a zero.** `subscriber_count: None` means the response did not state a
//!    subscriber count; `Some(Count::value(0))` would mean "this channel has no subscribers", which is
//!    a claim upstream almost never makes (hidden counts are rendered as text, not as `0`).
//! 2. **Counts keep their precision and their original text.** Channel pages render
//!    `1.24M subscribers`; that string is not an exact number and must never be presented as one, so
//!    the parsed value is [`crate::common::Precision::Approximate`] and `raw` keeps what YouTube said.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use vydra_common::ChannelId;

use crate::common::{Availability, Count, MetadataEvidence, Provenance, Thumbnail};

/// Compact channel representation used inside search results and video documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSummary {
    pub id: ChannelId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriber_count: Option<Count>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_count: Option<Count>,
}

/// A link on the channel page ("about" tab, external sites, social profiles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelLink {
    pub title: String,
    pub url: String,
}

/// A tab exposed by the channel page.
///
/// `slug` is only set for tabs Vydra actually recognizes; an unrecognized tab keeps its upstream title
/// and no slug, rather than a guessed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelTab {
    pub title: String,
    /// Stable slug used in the API (`videos`, `playlists`, `shorts`, `live`, …), when recognized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Whether upstream marked this tab as the currently selected one.
    #[serde(default)]
    pub selected: bool,
}

/// A verification indicator, exactly as the page renders it.
///
/// There is deliberately no `verified: bool` field: the absence of a badge is not proof of the absence
/// of verification (the page may not render badges for the requesting client, or the badge may be
/// rendered in a shape this parser does not know), so "no indicator" is expressed as an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelVerification {
    /// Badge style string from the header, e.g. `BADGE_STYLE_TYPE_VERIFIED`.
    pub style: String,
    /// Human label, when upstream provides one (e.g. `Official Artist Channel`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Document returned by `GET /api/v1/channels/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDocument {
    pub summary: ChannelSummary,
    pub availability: Availability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<Thumbnail>,
    /// Empty today: links live on the `about` tab, which this phase does not request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<ChannelLink>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub joined_at: Option<OffsetDateTime>,
    /// Empty today, for the same reason as `links`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Empty today, for the same reason as `links`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_view_count: Option<Count>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<ChannelTab>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<ChannelVerification>,
    /// Which upstream sections supplied the fields above (ADR-021), plus fallbacks and conflicts.
    #[serde(default, skip_serializing_if = "MetadataEvidence::is_empty")]
    pub evidence: MetadataEvidence,
    pub provenance: Provenance,
}

impl ChannelDocument {
    /// The channel id as validated by the domain (never the raw requested handle).
    pub fn id(&self) -> &ChannelId {
        &self.summary.id
    }

    /// Slug of the tab holding this channel's long-form video list, when the page has one.
    pub fn videos_tab_slug(&self) -> Option<&str> {
        self.tabs
            .iter()
            .find(|tab| tab.slug.as_deref() == Some("videos"))
            .and_then(|tab| tab.slug.as_deref())
    }
}

/// Paged list of a channel's videos (`GET /api/v1/channels/{id}/videos`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelVideoList {
    pub items: Vec<crate::video::VideoSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Entries dropped because the same video id appeared more than once in this page.
    ///
    /// Kept as a number instead of silently deduplicating: a page that reports `returned + duplicates`
    /// rows is a different fact from a page that reported fewer rows, and only one of them is a lie.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duplicates_removed: u32,
    pub provenance: Provenance,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}
