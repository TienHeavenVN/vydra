//! Shared value objects: provenance, counts, thumbnails, availability, media and captions.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use vydra_errors::UnavailabilityReason;

/// Where a document came from and how clean the parse was (ADR-016).
///
/// Every public document carries this. It is the mechanism by which Vydra can be honest about
/// degraded data instead of silently presenting it as authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Provider id, e.g. `innertube-web`.
    pub provider: String,
    /// Upstream endpoint family, e.g. `search`, `player`, `browse`.
    pub endpoint: String,
    /// Client profile used, when the provider is client-aware.
    pub client: Option<String>,
    /// When the upstream document was fetched (not when the response was served).
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// Whether the document was served from cache.
    pub cached: bool,
    /// Number of non-fatal parse warnings recorded while normalizing.
    pub parse_warnings: u32,
    /// Every provider attempt that contributed to this document (including failed fallbacks).
    ///
    /// Audit finding A-05: the registry tracked attempts but engines discarded them, so the public
    /// `meta.sources` array was permanently empty. It is now populated end to end.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceAttempt>,
}

/// How precise a numeric value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    /// Parsed from an exact integer (e.g. `viewCount` field).
    Exact,
    /// Parsed from an abbreviated human string (e.g. `1.2K views`) — rounded.
    Approximate,
}

/// A count that remembers where it came from.
///
/// `raw` is preserved so that a client can display exactly what YouTube rendered and an operator can
/// debug a parse without re-fetching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    pub value: u64,
    pub precision: Precision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl Count {
    pub fn exact(value: u64) -> Self {
        Self {
            value,
            precision: Precision::Exact,
            raw: None,
        }
    }

    pub fn approximate(value: u64, raw: impl Into<String>) -> Self {
        Self {
            value,
            precision: Precision::Approximate,
            raw: Some(raw.into()),
        }
    }
}

/// An image with optional intrinsic size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

impl Thumbnail {
    pub fn new(url: impl Into<String>, width: Option<u32>, height: Option<u32>) -> Self {
        Self {
            url: url.into(),
            width,
            height,
        }
    }
}

/// Availability of a resource, as distinct from its existence.
///
/// A private video exists (id is valid) but is unavailable; returning 404 for it would be a lie
/// (`docs/architecture/API_CONTRACTS.md` §1). `Available` with empty payload is never used as a
/// stand-in for unavailable content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Upcoming,
    Live,
    Unavailable {
        #[serde(rename = "reason")]
        reason: UnavailabilityReason,
    },
}

impl Availability {
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available | Self::Upcoming | Self::Live)
    }

    pub fn from_reason(reason: UnavailabilityReason) -> Self {
        Self::Unavailable { reason }
    }
}

/// Broadcast state of a video, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    NotLive,
    Live,
    Upcoming,
    Ended,
}

/// One chapter marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub start_seconds: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_seconds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<Thumbnail>,
}

/// How a media format URL is protected (ADR-006).
///
/// Vydra never returns media URLs that require circumventing an integrity control; it reports the
/// metadata and the reason a URL is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// Not inspected for URLs (metadata-only response).
    NotInspected,
    /// URL was present and directly usable.
    Direct,
    /// URL requires `signatureCipher` deciphering — omitted by policy.
    SignatureProtected,
    /// URL requires a proof-of-origin token — omitted by policy.
    TokenProtected,
    /// DRM protected — never obtainable.
    Drm,
    /// Provider did not report delivery information.
    Unknown,
}

/// Format metadata. Intentionally contains **no** media URL field (ADR-006).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFormatInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itag: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codecs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_quality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approx_duration_ms: Option<u64>,
    pub audio_only: bool,
    pub has_drm: bool,
    pub delivery: DeliveryKind,
}

/// Whether a caption track is manual or machine generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionKind {
    /// Uploaded by the creator.
    Manual,
    /// Auto-generated (`kind = "asr"` upstream).
    Automatic,
    /// Provider did not state it.
    Unknown,
}

/// Caption track metadata.
///
/// `base_url_present` records that the provider exposed a `timedtext` URL without echoing it: the
/// URL is request-scoped and may carry integrity parameters, so it is not part of the public model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionTrack {
    pub language_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: CaptionKind,
    pub is_translatable: bool,
    pub base_url_present: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub translation_languages: Vec<String>,
}

/// A single attempt made by the provider registry, surfaced in response `meta.sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAttempt {
    pub provider: String,
    pub endpoint: String,
    pub attempt: u8,
    pub latency_ms: u64,
    pub outcome: AttemptOutcome,
}

/// Result of one provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Ok,
    Empty,
    Skipped,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_serializes_with_reason() {
        let value = Availability::from_reason(UnavailabilityReason::Private);
        let json = serde_json::to_value(&value).expect("serialize");
        assert_eq!(json["state"], "unavailable");
        assert_eq!(json["reason"], "private");
        assert!(!value.is_available());
    }

    #[test]
    fn approximate_counts_preserve_raw_text() {
        let count = Count::approximate(1_200, "1.2K views");
        let json = serde_json::to_value(&count).expect("serialize");
        assert_eq!(json["precision"], "approximate");
        assert_eq!(json["raw"], "1.2K views");
    }

    #[test]
    fn exact_counts_omit_raw() {
        let json = serde_json::to_value(Count::exact(7)).expect("serialize");
        assert!(json.get("raw").is_none());
    }
}
