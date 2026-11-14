//! Provider-neutral domain models.
//!
//! These types are the public API contract. They must never contain provider-specific shapes,
//! provider-specific identifiers, media URLs, or fields whose absence would be indistinguishable
//! from zero (`docs/research/IMPLEMENTATION_DECISIONS.md` ADR-002 and ADR-016).
//!
//! Providers construct them; `vydra-api` serializes them; nothing else may change them.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod captions;
pub mod channel;
pub mod comment;
pub mod common;
pub mod playlist;
pub mod search;
pub mod video;

pub use captions::CaptionTrackList;
pub use channel::{
    ChannelDocument, ChannelLink, ChannelSummary, ChannelTab, ChannelVerification, ChannelVideoList,
};
pub use comment::{
    CommentAuthor, CommentDocument, CommentList, CommentSort, CommentThread, ReplyList,
};
pub use common::{
    AttemptOutcome, Availability, BroadcastInfo, CaptionKind, CaptionTrack, Chapter, Count,
    DeliveryKind, FieldConflict, FieldFallback, FieldSource, LiveState, MediaFormatInfo,
    MetadataEvidence, Precision, Provenance, Publication, SourceAttempt, Storyboard,
    StoryboardLevel, Thumbnail,
};
pub use playlist::{
    PlaylistDocument, PlaylistEntrySource, PlaylistItem, PlaylistSummary, PlaylistVideoList,
};
pub use search::{SearchItem, SearchResults};
pub use video::{RelatedVideos, VideoChapters, VideoDocument, VideoSummary};
pub use vydra_common::{ChannelId, CommentId, PlaylistId, VideoId};
pub use vydra_errors::UnavailabilityReason;
