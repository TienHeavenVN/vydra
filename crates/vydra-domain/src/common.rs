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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// Upstream answered without stating playability at all.
    ///
    /// Not a synonym for `Available`: a document in this state is never cached, never counted as a
    /// success and never treated as playable — it says exactly one thing, "the verdict is missing".
    /// Added in Phase 5A because the previous default (assume `Available` when the field was absent)
    /// manufactured a success the response never claimed; see `docs/reports/PHASE_5A_REPORT.md`.
    Unknown,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub itag: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codecs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: CaptionKind,
    pub is_translatable: bool,
    pub base_url_present: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub translation_languages: Vec<String>,
}

/// Which upstream section a value came from (ADR-021).
///
/// The player response duplicates several facts across sections — `lengthSeconds` in both
/// `videoDetails` and `microformat`, `viewCount` in both, the channel name in both. When they agree the
/// duplication is harmless; when they disagree the document has to say which one it used, because the
/// alternative is a number that cannot be explained afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    VideoDetails,
    Microformat,
    StreamingData,
    Captions,
    Storyboards,
    /// Derived from the description (chapter markers).
    Description,
    /// Supplied by the caller (the requested id).
    Request,
}

impl FieldSource {
    /// Upstream section name, for messages and documents.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VideoDetails => "videoDetails",
            Self::Microformat => "microformat",
            Self::StreamingData => "streamingData",
            Self::Captions => "captions",
            Self::Storyboards => "storyboards",
            Self::Description => "description",
            Self::Request => "request",
        }
    }
}

/// A field taken from a secondary section because the primary one did not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldFallback {
    /// Domain field name, e.g. `thumbnails`.
    pub field: String,
    /// Section the value actually came from.
    pub source: FieldSource,
}

/// Two upstream sections disagreed about one field.
///
/// `kept` is what the document reports; `rejected` is what the other section said. Both are kept as
/// text so a client can audit the disagreement without the raw response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldConflict {
    pub field: String,
    pub kept: FieldSource,
    pub kept_value: String,
    pub rejected: FieldSource,
    pub rejected_value: String,
}

/// What the response contained and where the document's values came from.
///
/// This is the "confidence" half of a document: it does not estimate a probability, it states the
/// evidence. A field with no conflict, no fallback and a present section is as strong as this provider
/// can make it; anything else is visible here instead of being silently averaged away.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataEvidence {
    /// Sections present in the payload, in document order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<FieldSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<FieldFallback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<FieldConflict>,
}

impl MetadataEvidence {
    pub fn section(&mut self, source: FieldSource) {
        if !self.sections.contains(&source) {
            self.sections.push(source);
        }
    }

    pub fn fallback(&mut self, field: &str, source: FieldSource) {
        self.fallbacks.push(FieldFallback {
            field: field.to_owned(),
            source,
        });
    }

    pub fn conflict(&mut self, conflict: FieldConflict) {
        self.conflicts.push(conflict);
    }

    pub fn has_section(&self, source: FieldSource) -> bool {
        self.sections.contains(&source)
    }

    /// Whether nothing was recorded (used to omit the object from the wire form).
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.fallbacks.is_empty() && self.conflicts.is_empty()
    }
}

/// Publication timestamps with the upstream text preserved.
///
/// ADR-016 requires RFC 3339 **UTC** for every timestamp, and the live player capture publishes
/// `2009-10-24T23:57:33-07:00`. Normalising is right — one instant must not have two wire forms — but
/// normalising is lossy unless the original is kept, so it is: `upstream_text` is verbatim and
/// `upstream_offset_seconds` records the publisher's offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// Published instant, normalised to UTC.
    #[serde(with = "time::serde::rfc3339")]
    pub published_at: OffsetDateTime,
    /// The upstream string exactly as it arrived, offset included.
    pub upstream_text: String,
    /// Offset the publisher stated, in seconds east of UTC (`-07:00` is `-25200`).
    pub upstream_offset_seconds: i32,
    /// `microformat.uploadDate` when it differs from (or accompanies) `publishDate`, normalised to UTC.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub uploaded_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_text: Option<String>,
}

/// Live-broadcast window, from `microformat.liveBroadcastDetails`.
///
/// Only present when upstream states it; a regular video has no broadcast object at all rather than one
/// with invented timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_now: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub start_at: Option<OffsetDateTime>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub end_at: Option<OffsetDateTime>,
}

/// One storyboard level: a grid of thumbnails covering the video at a fixed interval.
///
/// The upstream `spec` string also carries a URL template with an image signature. Only the geometry is
/// modelled: the template is not stored (it is a signed URL, ADR-006), and `url_template_present`
/// records that it existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryboardLevel {
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    pub columns: u32,
    pub rows: u32,
    pub interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The level carried a signature field. Never echoed.
    pub sigh_present: bool,
}

/// Storyboard metadata for a video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storyboard {
    /// Ordered as upstream listed them (coarsest first).
    pub levels: Vec<StoryboardLevel>,
    pub url_template_present: bool,
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
    use time::format_description::well_known::Rfc3339;
    use time::UtcOffset;

    #[test]
    fn availability_serializes_with_reason() {
        let value = Availability::from_reason(UnavailabilityReason::Private);
        let json = serde_json::to_value(value).expect("serialize");
        assert_eq!(json["state"], "unavailable");
        assert_eq!(json["reason"], "private");
        assert!(!value.is_available());
    }

    #[test]
    fn unknown_availability_is_never_available() {
        let json = serde_json::to_value(Availability::Unknown).expect("serialize");
        assert_eq!(json["state"], "unknown");
        assert!(
            !Availability::Unknown.is_available(),
            "a missing verdict must never be cached or served as playable"
        );
        // The other states keep their wire form; the variant is additive, not a rename.
        let available = serde_json::to_value(Availability::Available).expect("serialize");
        assert_eq!(available["state"], "available");
    }

    #[test]
    fn approximate_counts_preserve_raw_text() {
        let count = Count::approximate(1_200, "1.2K views");
        let json = serde_json::to_value(count).expect("serialize");
        assert_eq!(json["precision"], "approximate");
        assert_eq!(json["raw"], "1.2K views");
    }

    #[test]
    fn publication_keeps_the_upstream_text_and_offset() {
        let text = "2009-10-24T23:57:33-07:00";
        let parsed = OffsetDateTime::parse(text, &Rfc3339).expect("timestamp");
        let publication = Publication {
            published_at: parsed.to_offset(UtcOffset::UTC),
            upstream_text: text.to_owned(),
            upstream_offset_seconds: parsed.offset().whole_seconds(),
            uploaded_at: None,
            uploaded_text: None,
        };
        let json = serde_json::to_value(&publication).expect("serialize");
        // The instant is UTC on the wire; the publisher's offset survives as data, not as format.
        assert_eq!(json["published_at"], "2009-10-25T06:57:33Z");
        assert_eq!(json["upstream_text"], text);
        assert_eq!(json["upstream_offset_seconds"], -25200);
        assert!(json.get("uploaded_at").is_none());
    }

    #[test]
    fn evidence_omits_empty_arrays_and_records_conflicts() {
        let mut evidence = MetadataEvidence::default();
        evidence.section(FieldSource::VideoDetails);
        evidence.section(FieldSource::VideoDetails);
        evidence.fallback("thumbnails", FieldSource::VideoDetails);
        evidence.conflict(FieldConflict {
            field: "duration_seconds".into(),
            kept: FieldSource::VideoDetails,
            kept_value: "213".into(),
            rejected: FieldSource::Microformat,
            rejected_value: "214".into(),
        });

        let json = serde_json::to_value(&evidence).expect("serialize");
        assert_eq!(json["sections"], serde_json::json!(["video_details"]));
        assert_eq!(json["fallbacks"][0]["source"], "video_details");
        assert_eq!(json["conflicts"][0]["kept_value"], "213");
        assert_eq!(json["conflicts"][0]["rejected"], "microformat");

        let empty = serde_json::to_value(MetadataEvidence::default()).expect("serialize");
        assert_eq!(empty, serde_json::json!({}));
    }

    #[test]
    fn exact_counts_omit_raw() {
        let json = serde_json::to_value(Count::exact(7)).expect("serialize");
        assert!(json.get("raw").is_none());
    }
}
