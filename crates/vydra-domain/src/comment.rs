//! Comment documents.
//!
//! Normative contract: `docs/research/COMMENTS_REQUEST_CONTRACT.md` and
//! `docs/research/COMMENTS_RESPONSE_ANALYSIS.md`.
//!
//! Five rules shape this module, and each one exists because the obvious alternative fabricates data:
//!
//! 1. **A missing count is not a zero.** `like_count: None` means the response did not state a like count;
//!    `Some(Count::exact(0))` means it stated zero. The same distinction applies to `reply_count`, which is
//!    why an unloaded reply is never reported as "no replies".
//! 2. **A display name is not an identifier.** `CommentAuthor::channel_id` is `None` unless the response
//!    carried a validated `UC…` channel id. No id is derived from a name, and a name is never parsed as a
//!    handle.
//! 3. **Localized text is preserved verbatim.** `published_text` keeps what the page rendered
//!    (`"2 days ago"`, `"vor 2 Tagen"`); `published_at` exists for providers that state an absolute time and
//!    is `None` whenever only relative text was rendered.
//! 4. **Partial data survives.** A malformed member costs the member, not the comment: a comment whose
//!    avatar list is unreadable is served with no avatars, and the loss is reported as a parse warning rather
//!    than swallowed.
//! 5. **Availability is stated, not assumed.** A comment row that says nothing about its own state is
//!    `Availability::Unknown`, and `Unknown` is never treated as playable or as an error.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use vydra_common::{ChannelId, CommentId};

use crate::common::{Availability, Count, Provenance, Thumbnail};

/// Author of a comment, as the page rendered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentAuthor {
    /// Validated `UC…` channel id, when the response carried one.
    ///
    /// `None` is not "a channel without an id": it means the response did not state an id, or stated one
    /// that failed validation. Nothing is derived from [`Self::display_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<ChannelId>,
    /// Display name exactly as rendered. Never empty: a comment without a name is not emitted.
    pub display_name: String,
    /// Avatars, ordered smallest → largest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avatars: Vec<Thumbnail>,
    /// The comment's author is the owner of the video being commented on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_creator: bool,
    /// The channel carries a verification badge.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_verified: bool,
    /// The author carries a membership (sponsor) badge.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_member: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentDocument {
    pub id: CommentId,
    /// Comment text as plain text. Line breaks are preserved; markup is not interpreted.
    pub text: String,
    pub author: CommentAuthor,
    /// Relative publication text exactly as rendered (`"2 days ago"`).
    pub published_text: String,
    /// Absolute publication time, only when the response stated one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub published_at: Option<OffsetDateTime>,
    /// Likes as stated. `None` when the row states none; `Approximate` when the label was abbreviated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_count: Option<Count>,
    /// Replies as stated by the row. `None` when the row states no reply count.
    ///
    /// This is the *total* the page claims, which is not the number of replies Vydra has loaded. A comment
    /// with `reply_count: Some(14)` and no loaded replies has fourteen replies that have not been fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_count: Option<u32>,
    /// The video's owner pinned this comment.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_pinned: bool,
    /// The video's owner hearted this comment.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_hearted: bool,
    /// Parent comment id for a reply; `None` for a top-level comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<CommentId>,
    /// Per-comment availability. `Unknown` means the row stated nothing.
    pub availability: Availability,
}

/// A top-level comment together with the replies that came with it.
///
/// The three reply facts are deliberately separate, because conflating them is how "we did not fetch it"
/// becomes "it does not exist":
///
/// * [`Self::total_reply_count`] — what the row claimed;
/// * [`Self::loaded_replies`] — what this response actually contained;
/// * [`Self::has_more_replies`] — whether the response offered a continuation for more replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentThread {
    pub comment: CommentDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_reply_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaded_replies: Vec<CommentDocument>,
    /// Whether upstream offered a continuation for further replies.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_more_replies: bool,
}

impl CommentThread {
    /// Replies that the row claims but this document does not carry.
    ///
    /// `None` when the row stated no count, or when the statement is already satisfied. Never negative, and
    /// never zero merely because nothing was loaded.
    pub fn unloaded_replies(&self) -> Option<u32> {
        let total = self.total_reply_count?;
        let loaded = u32::try_from(self.loaded_replies.len()).unwrap_or(u32::MAX);
        total.checked_sub(loaded).filter(|remaining| *remaining > 0)
    }
}

/// Comment sort order.
///
/// Only the orders upstream itself offers are modelled. The tokens are relayed from the page's own sort
/// menu (never encoded by Vydra), and an order upstream does not offer is `Unsupported`, not "HTTP 200, so
/// it probably worked".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentSort {
    /// "Top comments" — the default the page opens with.
    Top,
    /// "Newest first".
    Newest,
}

impl CommentSort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Newest => "newest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "top" => Some(Self::Top),
            "newest" => Some(Self::Newest),
            _ => None,
        }
    }
}

/// One page of top-level comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentList {
    pub items: Vec<CommentThread>,
    /// The order this page was fetched in.
    pub sort: CommentSort,
    /// Upstream continuation token for the next page. The provider never exposes a cursor; the engine
    /// wraps this token in a signed envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_token: Option<String>,
    /// How many rows in this page repeated a comment id already seen in this page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duplicates: u32,
    pub availability: Availability,
    pub provenance: Provenance,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// One page of replies under a single parent comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyList {
    /// The comment these replies belong to. Always present: a reply page is never served without knowing
    /// which thread it belongs to.
    pub parent_id: CommentId,
    pub items: Vec<CommentDocument>,
    /// Total the page claims for the parent, when it restates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_reply_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_token: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub duplicates: u32,
    pub availability: Availability,
    pub provenance: Provenance,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use serde_json::{Value, json};
    use time::macros::datetime;

    use super::*;

    fn author(channel: Option<&str>) -> CommentAuthor {
        CommentAuthor {
            channel_id: channel.and_then(|id| ChannelId::parse(id).ok()),
            display_name: "Example Commenter".into(),
            avatars: vec![],
            is_creator: false,
            is_verified: false,
            is_member: false,
        }
    }

    fn comment(id: &str) -> CommentDocument {
        CommentDocument {
            id: CommentId::parse(id).expect("valid comment id"),
            text: "a comment".into(),
            author: author(Some("UCaaaaaaaaaaaaaaaaaaaaaa")),
            published_text: "2 days ago".into(),
            published_at: None,
            like_count: None,
            reply_count: None,
            is_pinned: false,
            is_hearted: false,
            parent_id: None,
            availability: Availability::Available,
        }
    }

    fn provenance() -> Provenance {
        Provenance {
            provider: "innertube-web".into(),
            endpoint: "next".into(),
            client: Some("web".into()),
            fetched_at: datetime!(2026-11-14 00:00 UTC),
            cached: false,
            parse_warnings: 0,
            sources: vec![],
        }
    }

    #[test]
    fn a_missing_like_count_is_not_zero_likes() {
        let json = serde_json::to_string(&comment("Ugx000000000000000000000001")).expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("parse");
        assert!(value.get("like_count").is_none(), "{value}");
        assert!(value.get("reply_count").is_none(), "{value}");

        let back: CommentDocument = serde_json::from_str(&json).expect("deserialize");
        assert!(back.like_count.is_none());
        assert!(back.reply_count.is_none());
    }

    #[test]
    fn a_display_name_never_becomes_an_author_id() {
        let document = CommentDocument {
            author: author(None),
            ..comment("Ugx000000000000000000000002")
        };
        let json = serde_json::to_string(&document).expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("parse");
        assert!(value["author"].get("channel_id").is_none(), "{value}");
        assert_eq!(value["author"]["display_name"], json!("Example Commenter"));
    }

    #[test]
    fn an_unloaded_reply_is_not_a_missing_one() {
        let thread = CommentThread {
            comment: CommentDocument {
                reply_count: Some(14),
                ..comment("Ugx000000000000000000000003")
            },
            total_reply_count: Some(14),
            loaded_replies: vec![],
            has_more_replies: true,
        };
        assert_eq!(thread.unloaded_replies(), Some(14));

        // Nothing claimed, nothing loaded: "unknown", not "zero".
        let quiet = CommentThread {
            comment: comment("Ugx000000000000000000000004"),
            total_reply_count: None,
            loaded_replies: vec![],
            has_more_replies: false,
        };
        assert_eq!(quiet.unloaded_replies(), None);
    }

    #[test]
    fn flags_are_omitted_when_false_and_kept_when_true() {
        let mut pinned = comment("Ugx000000000000000000000005");
        pinned.is_pinned = true;
        pinned.is_hearted = true;
        let value: Value =
            serde_json::from_str(&serde_json::to_string(&pinned).expect("serialize")).expect("parse");
        assert_eq!(value["is_pinned"], json!(true));
        assert_eq!(value["is_hearted"], json!(true));

        let plain: Value = serde_json::from_str(
            &serde_json::to_string(&comment("Ugx000000000000000000000006")).expect("serialize"),
        )
        .expect("parse");
        assert!(plain.get("is_pinned").is_none(), "{plain}");
        assert!(plain.get("is_hearted").is_none(), "{plain}");
    }

    #[test]
    fn a_reply_page_always_names_its_parent() {
        let page = ReplyList {
            parent_id: CommentId::parse("Ugx000000000000000000000007").expect("valid id"),
            items: vec![],
            total_reply_count: Some(3),
            next_token: None,
            duplicates: 0,
            availability: Availability::Unknown,
            provenance: provenance(),
        };
        let value: Value =
            serde_json::from_str(&serde_json::to_string(&page).expect("serialize")).expect("parse");
        assert_eq!(value["parent_id"], json!("Ugx000000000000000000000007"));
        assert_eq!(value["total_reply_count"], json!(3));
        assert!(value.get("next_token").is_none(), "{value}");
        assert!(value.get("duplicates").is_none(), "{value}");
    }

    #[test]
    fn sort_round_trips_over_the_wire_names_clients_use() {
        assert_eq!(CommentSort::parse("top"), Some(CommentSort::Top));
        assert_eq!(CommentSort::parse("newest"), Some(CommentSort::Newest));
        assert_eq!(CommentSort::parse("TOP_COMMENTS"), None);
        let value: Value =
            serde_json::from_str(&serde_json::to_string(&CommentSort::Newest).expect("serialize"))
                .expect("parse");
        assert_eq!(value, json!("newest"));
    }
}
