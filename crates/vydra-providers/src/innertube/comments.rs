//! Comment mapping: the `next` endpoint's comment section, in both renderer shapes it uses.
//!
//! Normative contract: `docs/research/COMMENTS_RESPONSE_ANALYSIS.md`. The wire facts this module relies on
//! were read from three reference implementations and then checked against a live capture (Phase 7 probe):
//!
//! * the comment section is reached through a continuation token that the **watch page itself** renders in
//!   `itemSectionRenderer.targetId == "comments-section"`; Vydra never encodes that token;
//! * one page arrives as `onResponseReceivedEndpoints[last]` → `reloadContinuationItemsCommand` *or*
//!   `appendContinuationItemsAction` → `continuationItems[]`;
//! * a comment arrives either as the classic `commentThreadRenderer.comment.commentRenderer`, or as the
//!   current `commentThreadRenderer.commentViewModel` plus a matching entry in
//!   `frameworkUpdates.entityBatchUpdate.mutations[]`; replies arrive as bare `commentRenderer` /
//!   `commentViewModel` rows;
//! * the page's own sort menu (`commentsHeaderRenderer.sortMenu.sortFilterSubMenuRenderer.subMenuItems[]`)
//!   carries one continuation token per order. Those tokens are relayed, never constructed.
//!
//! Why this module walks the payload itself instead of using [`crate::parse::collect_renderers`]: a comment
//! page is full of chrome (`buttonViewModel`, `listItemViewModel`, `chipViewModel`, …) whose renderer keys
//! are not content and would drown the report. What matters here is the *item array*, so the walk is scoped
//! to it and everything inside it that is not a comment is counted with its key.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;
use vydra_common::{ChannelId, CommentId};
use vydra_domain::{
    Availability, CommentAuthor, CommentDocument, CommentList, CommentSort, CommentThread,
    Provenance, ReplyList, Thumbnail, UnavailabilityReason,
};
use vydra_errors::ProviderError;

use crate::innertube::map::count_from_text;
use crate::parse::{ParseReport, text_of, thumbnails_of};

/// Renderers that hold no comment and no comment machinery.
const STRUCTURAL: &[&str] = &[
    "commentRepliesRenderer",
    "commentThreadRenderer",
    "commentsHeaderRenderer",
    "commentsSimpleboxRenderer",
    "commentSimpleboxRenderer",
    "commentActionButtonsRenderer",
    "commentDialogRenderer",
    "commentsEntryPointHeaderRenderer",
    "sectionListRenderer",
    "itemSectionRenderer",
    "continuationItemRenderer",
];

/// The continuation token of the video's comment section, from a watch (`next`) response.
///
/// `Ok(None)` means the watch page rendered **no** comment section, which is how upstream says "comments are
/// turned off": the section is absent rather than empty. `Err` means the payload is not a watch page at all.
pub fn comments_section_token(payload: &Value) -> Result<Option<String>, ProviderError> {
    let contents = payload
        .get("contents")
        .ok_or_else(|| ProviderError::Parse {
            message: "next response has no contents".to_owned(),
        })?;

    let mut stack = vec![contents];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "itemSectionRenderer" {
                        let target = value.get("targetId").and_then(Value::as_str);
                        let section = value.get("sectionIdentifier").and_then(Value::as_str);
                        let is_comments = target == Some("comments-section")
                            || section == Some("comment-item-section");
                        if is_comments
                            && let Some(token) = value
                                .get("contents")
                                .and_then(Value::as_array)
                                .and_then(|items| {
                                    items.iter().find_map(|item| continuation_token(item))
                                })
                        {
                            return Ok(Some(token));
                        }
                    }
                    if value.is_object() || value.is_array() {
                        stack.push(value);
                    }
                }
            }
            Value::Array(items) => stack.extend(items.iter().filter(|item| item.is_object())),
            _ => {}
        }
    }
    Ok(None)
}

/// One entry of the sort menu as the page renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortOption {
    /// Rendered label (`"Top comments"`, `"Top-Kommentare"`). Kept because it is localized.
    pub title: String,
    /// Relayed continuation token for this order.
    pub token: String,
    /// Whether the page had this order selected.
    pub selected: bool,
}

/// The comment section's sort menu, in document order.
///
/// The order's *meaning* is positional: the first entry is "Top comments" and the second is "Newest first",
/// which is what every reference implementation reads and what the probe confirmed by title. A menu with one
/// entry therefore offers one order, and Vydra refuses the other rather than sending a token it cannot name.
pub fn sort_options(payload: &Value) -> Vec<SortOption> {
    let mut out = Vec::new();
    let mut stack = vec![payload];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => {
                if let Some(menu) = map.get("sortFilterSubMenuRenderer")
                    && let Some(items) = menu.get("subMenuItems").and_then(Value::as_array)
                {
                    for item in items {
                        let Some(title) = item.get("title").and_then(text_of) else {
                            continue;
                        };
                        let Some(token) = item.get("serviceEndpoint").and_then(continuation_token)
                        else {
                            continue;
                        };
                        out.push(SortOption {
                            title,
                            token,
                            selected: item
                                .get("selected")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        });
                    }
                    if !out.is_empty() {
                        return out;
                    }
                }
                for value in map.values() {
                    if value.is_object() || value.is_array() {
                        stack.push(value);
                    }
                }
            }
            Value::Array(items) => stack.extend(items.iter().filter(|item| item.is_object())),
            _ => {}
        }
    }
    out
}

/// Resolve the token to fetch `sort` with, given the menu the page rendered.
///
/// Returns `Err(Unsupported)` when the page does not offer that order: an unverified order is refused, not
/// approximated with the default one.
pub fn sort_token(
    options: &[SortOption],
    sort: CommentSort,
) -> Result<String, ProviderError> {
    let index = match sort {
        CommentSort::Top => 0,
        CommentSort::Newest => 1,
    };
    options
        .get(index)
        .map(|option| option.token.clone())
        .ok_or_else(|| ProviderError::Unsupported {
            capability: format!("comments_sort:{}", sort.as_str()),
        })
}

/// Map one page of top-level comments.
pub fn map_comments_page(
    payload: &Value,
    sort: CommentSort,
    limit: u16,
    report: &mut ParseReport,
    provenance: Provenance,
) -> CommentList {
    let items = continuation_items(payload);
    let entities = entity_map(payload);

    let mut threads = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0u32;
    let mut next_token = None;

    for item in items {
        if let Some(token) = continuation_token(item) {
            // The page's own next-page token. A page carries at most one; a second one is not a page
            // cursor and is reported rather than used.
            if next_token.is_none() {
                next_token = Some(token);
            } else {
                report.warn("comments_extra_continuation");
            }
            continue;
        }
        let Some(key) = item.as_object().and_then(|map| map.keys().next()).cloned() else {
            report.unmapped_result("comment_row_empty");
            continue;
        };
        let Some(thread) = map_thread(item, &key, &entities, report) else {
            continue;
        };
        if seen.insert(thread.comment.id.as_str().to_owned()) {
            threads.push(thread);
        } else {
            duplicates += 1;
        }
    }

    let truncated = threads.len() > usize::from(limit);
    if truncated {
        threads.truncate(usize::from(limit));
        report.warn("page_truncated");
    }

    CommentList {
        items: threads,
        sort,
        next_token,
        duplicates,
        availability: Availability::Available,
        provenance: Provenance {
            parse_warnings: report.count(),
            ..provenance
        },
    }
}

/// Map one page of replies under `parent`.
pub fn map_replies_page(
    payload: &Value,
    parent: &CommentId,
    limit: u16,
    report: &mut ParseReport,
    provenance: Provenance,
) -> ReplyList {
    let items = continuation_items(payload);
    let entities = entity_map(payload);

    let mut replies = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0u32;
    let mut next_token = None;

    for item in items {
        if let Some(token) = continuation_token(item) {
            if next_token.is_none() {
                next_token = Some(token);
            } else {
                report.warn("comments_extra_continuation");
            }
            continue;
        }
        let Some(key) = item.as_object().and_then(|map| map.keys().next()).cloned() else {
            report.unmapped_result("comment_row_empty");
            continue;
        };
        let Some(mut comment) = comment_from_row(item, &key, &entities, report) else {
            continue;
        };
        if comment.parent_id.is_none() {
            comment.parent_id = Some(parent.clone());
        }
        if seen.insert(comment.id.as_str().to_owned()) {
            replies.push(comment);
        } else {
            duplicates += 1;
        }
    }

    let truncated = replies.len() > usize::from(limit);
    if truncated {
        replies.truncate(usize::from(limit));
        report.warn("page_truncated");
    }

    ReplyList {
        parent_id: parent.clone(),
        // A reply page does not restate the thread's total: the parent's `reply_count` is the only
        // statement of it, and inventing one here would be a second source of truth.
        total_reply_count: None,
        items: replies,
        next_token,
        duplicates,
        availability: Availability::Available,
        provenance: Provenance {
            parse_warnings: report.count(),
            ..provenance
        },
    }
}

/// `onResponseReceivedEndpoints[last].continuationItems[]`.
///
/// The *last* endpoint carries the page body; the first one carries the header (sort menu) on the initial
/// response. Some responses have only one endpoint, which is why the walk takes the last non-empty array.
fn continuation_items(payload: &Value) -> Vec<&Value> {
    let Some(endpoints) = payload.get("onResponseReceivedEndpoints").and_then(Value::as_array) else {
        return Vec::new();
    };
    for endpoint in endpoints.iter().rev() {
        for key in [
            "reloadContinuationItemsCommand",
            "appendContinuationItemsAction",
        ] {
            if let Some(items) = endpoint
                .get(key)
                .and_then(|command| command.get("continuationItems"))
                .and_then(Value::as_array)
            {
                return items.iter().collect();
            }
        }
    }
    Vec::new()
}

/// `frameworkUpdates.entityBatchUpdate.mutations[]` as `entityKey → payload`.
fn entity_map(payload: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(mutations) = payload
        .get("frameworkUpdates")
        .and_then(|updates| updates.get("entityBatchUpdate"))
        .and_then(|batch| batch.get("mutations"))
        .and_then(Value::as_array)
    else {
        return out;
    };
    for mutation in mutations {
        let Some(key) = mutation.get("entityKey").and_then(Value::as_str) else {
            continue;
        };
        if let Some(payload) = mutation.get("payload") {
            out.insert(key.to_owned(), payload.clone());
        }
    }
    out
}

/// `continuationItemRenderer` → its continuation token, in either of the two shapes the page uses.
fn continuation_token(item: &Value) -> Option<String> {
    let renderer = item.get("continuationItemRenderer")?;
    let token = renderer
        .get("continuationEndpoint")
        .and_then(|endpoint| endpoint.get("continuationCommand"))
        .and_then(|command| command.get("token"))
        .and_then(Value::as_str)
        .or_else(|| {
            renderer
                .get("button")
                .and_then(|button| button.get("buttonRenderer"))
                .and_then(|button| button.get("command"))
                .and_then(|command| command.get("continuationCommand"))
                .and_then(|command| command.get("token"))
                .and_then(Value::as_str)
        })?;
    // Tokens are opaque; the only thing worth checking is that it is a plausible relayed blob and not an
    // empty string pretending to be one.
    (!token.is_empty() && token.len() <= 4096).then(|| token.to_owned())
}

fn map_thread(
    item: &Value,
    key: &str,
    entities: &BTreeMap<String, Value>,
    report: &mut ParseReport,
) -> Option<CommentThread> {
    let thread = item.get("commentThreadRenderer")?;
    let comment = comment_from_row(item, key, entities, report)?;
    let parent_id = comment.id.clone();

    let mut loaded_replies = Vec::new();
    let mut has_more_replies = false;
    if let Some(replies) = thread
        .get("replies")
        .and_then(|replies| replies.get("commentRepliesRenderer"))
    {
        if let Some(items) = replies.get("contents").and_then(Value::as_array) {
            for reply in items {
                if continuation_token(reply).is_some() {
                    has_more_replies = true;
                    continue;
                }
                let Some(key) = reply.as_object().and_then(|map| map.keys().next()).cloned() else {
                    continue;
                };
                let Some(mut comment) = comment_from_row(reply, &key, entities, report) else {
                    continue;
                };
                if comment.parent_id.is_none() {
                    comment.parent_id = Some(parent_id.clone());
                }
                loaded_replies.push(comment);
            }
        }
    }

    Some(CommentThread {
        total_reply_count: comment.reply_count,
        comment,
        loaded_replies,
        has_more_replies,
    })
}

/// One comment row in any of the four shapes upstream uses.
fn comment_from_row(
    item: &Value,
    key: &str,
    entities: &BTreeMap<String, Value>,
    report: &mut ParseReport,
) -> Option<CommentDocument> {
    match key {
        "commentThreadRenderer" => {
            let thread = item.get("commentThreadRenderer")?;
            if let Some(view) = thread.get("commentViewModel") {
                let view = view.get("commentViewModel").unwrap_or(view);
                return comment_from_view_model(view, entities, report);
            }
            if let Some(renderer) = thread
                .get("comment")
                .and_then(|comment| comment.get("commentRenderer"))
            {
                return comment_from_renderer(renderer, report);
            }
            report.unmapped_result("commentThreadRenderer_without_body");
            None
        }
        "commentViewModel" => {
            let view = item.get("commentViewModel")?;
            comment_from_view_model(view, entities, report)
        }
        "commentRenderer" => {
            let renderer = item.get("commentRenderer")?;
            comment_from_renderer(renderer, report)
        }
        other if STRUCTURAL.contains(&other) => None,
        other => {
            report.unmapped_result(other);
            None
        }
    }
}

/// The current shape: a `commentViewModel` whose data lives in the entity batch update.
fn comment_from_view_model(
    view: &Value,
    entities: &BTreeMap<String, Value>,
    report: &mut ParseReport,
) -> Option<CommentDocument> {
    let comment_key = view.get("commentKey").and_then(Value::as_str).unwrap_or_default();
    let entity = entities
        .get(comment_key)
        .and_then(|payload| payload.get("commentEntityPayload"));

    let toolbar = entity
        .and_then(|entity| entity.get("toolbar"))
        .cloned()
        .unwrap_or(Value::Null);
    let properties = entity
        .and_then(|entity| entity.get("properties"))
        .cloned()
        .unwrap_or(Value::Null);
    let author = entity
        .and_then(|entity| entity.get("author"))
        .cloned()
        .unwrap_or(Value::Null);

    let raw_id = properties
        .get("commentId")
        .and_then(Value::as_str)
        .or_else(|| view.get("commentId").and_then(Value::as_str));
    let Some(raw_id) = raw_id else {
        report.skipped_item("comment_view_model_without_id");
        return None;
    };
    let id = match CommentId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            report.skipped_item("comment_view_model_id_invalid");
            return None;
        }
    };

    let Some(display_name) = author.get("displayName").and_then(Value::as_str).map(str::to_owned)
    else {
        report.missing_field("commentViewModel", "author.displayName");
        return None;
    };

    let text = properties
        .get("content")
        .and_then(|content| content.get("content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| properties.get("content").and_then(text_of))
        .unwrap_or_default();
    if text.is_empty() {
        // A comment with no readable text is a deleted comment upstream renders as an empty body; it is
        // kept (so the thread keeps its place) but its availability says so.
        report.warn("comment_text_empty");
    }

    let channel_id_raw = author.get("channelId").and_then(Value::as_str).or_else(|| {
        author
            .get("channelCommand")
            .and_then(|command| command.get("innertubeCommand"))
            .and_then(|command| command.get("browseEndpoint"))
            .and_then(|endpoint| endpoint.get("browseId"))
            .and_then(Value::as_str)
    });
    let channel_id = channel_id_raw.and_then(|raw| ChannelId::parse(raw).ok());
    if channel_id_raw.is_some() && channel_id.is_none() {
        report.warn("comment_author_id_invalid");
    }

    let avatars = entity
        .and_then(|entity| entity.get("avatar"))
        .and_then(|avatar| avatar.get("image"))
        .and_then(|image| image.get("sources"))
        .map(thumbnails_of)
        .filter(|avatars| !avatars.is_empty())
        .or_else(|| {
            author
                .get("avatarThumbnailUrl")
                .and_then(Value::as_str)
                .map(|url| vec![Thumbnail::new(url, None, None)])
        })
        .unwrap_or_default();

    let hearted = view
        .get("toolbarStateKey")
        .and_then(Value::as_str)
        .and_then(|key| entities.get(key))
        .and_then(|payload| payload.get("engagementToolbarStateEntityPayload"))
        .and_then(|state| state.get("heartState"))
        .and_then(Value::as_str)
        == Some("TOOLBAR_HEART_STATE_HEARTED");

    let reply_count = toolbar
        .get("replyCount")
        .and_then(Value::as_str)
        .and_then(|text| count_from_text(text))
        .and_then(|count| u32::try_from(count.value).ok());

    let availability = if text.is_empty() && channel_id.is_none() {
        Availability::Unavailable {
            reason: UnavailabilityReason::Deleted,
        }
    } else {
        Availability::Available
    };

    Some(CommentDocument {
        id,
        text,
        author: CommentAuthor {
            channel_id,
            display_name,
            avatars,
            is_creator: author.get("isCreator").and_then(Value::as_bool).unwrap_or(false),
            is_verified: author.get("isVerified").and_then(Value::as_bool).unwrap_or(false)
                || author.get("isArtist").and_then(Value::as_bool).unwrap_or(false),
            is_member: author
                .get("sponsorBadgeUrl")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.is_empty()),
        },
        published_text: properties
            .get("publishedTime")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        published_at: None,
        like_count: toolbar
            .get("likeCountNotliked")
            .and_then(Value::as_str)
            .and_then(count_from_text),
        reply_count,
        is_pinned: view
            .get("pinnedText")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        is_hearted: hearted,
        parent_id: None,
        availability,
    })
}

/// The classic shape: everything is inside `commentRenderer`.
fn comment_from_renderer(
    renderer: &Value,
    report: &mut ParseReport,
) -> Option<CommentDocument> {
    let Some(raw_id) = renderer.get("commentId").and_then(Value::as_str) else {
        report.skipped_item("comment_renderer_without_id");
        return None;
    };
    let id = match CommentId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            report.skipped_item("comment_renderer_id_invalid");
            return None;
        }
    };

    let display_name = renderer
        .get("authorText")
        .and_then(text_of)
        .unwrap_or_default();
    if display_name.is_empty() {
        // Deleted comments keep their row and lose their author. The row is kept: dropping it would
        // renumber the page.
        report.warn("comment_author_missing");
    }

    let text = renderer.get("contentText").and_then(text_of).unwrap_or_default();
    let channel_id = renderer
        .get("authorEndpoint")
        .and_then(|endpoint| endpoint.get("browseEndpoint"))
        .and_then(|endpoint| endpoint.get("browseId"))
        .and_then(Value::as_str)
        .and_then(|raw| ChannelId::parse(raw).ok());

    let hearted = renderer
        .get("actionButtons")
        .and_then(|buttons| buttons.get("commentActionButtonsRenderer"))
        .and_then(|buttons| buttons.get("creatorHeart"))
        .is_some_and(|heart| !heart.is_null());

    let availability = if display_name.is_empty() && text.is_empty() {
        Availability::Unavailable {
            reason: UnavailabilityReason::Deleted,
        }
    } else {
        Availability::Available
    };

    Some(CommentDocument {
        id,
        text,
        author: CommentAuthor {
            channel_id,
            display_name,
            avatars: renderer
                .get("authorThumbnail")
                .map(thumbnails_of)
                .unwrap_or_default(),
            is_creator: renderer
                .get("authorIsChannelOwner")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            is_verified: renderer
                .get("authorCommentBadge")
                .is_some_and(|badge| !badge.is_null()),
            is_member: renderer
                .get("sponsorCommentBadge")
                .is_some_and(|badge| !badge.is_null()),
        },
        published_text: renderer
            .get("publishedTimeText")
            .and_then(text_of)
            .unwrap_or_default(),
        published_at: None,
        like_count: renderer
            .get("voteCount")
            .and_then(text_of)
            .as_deref()
            .and_then(count_from_text),
        // The classic renderer states no numeric reply count; the "View N replies" label is localized and
        // parsing it would be a guess, so the count stays `None`.
        reply_count: None,
        is_pinned: renderer
            .get("pinnedCommentBadge")
            .is_some_and(|badge| !badge.is_null()),
        is_hearted: hearted,
        parent_id: None,
        availability,
    })
}

/// The reply continuation token of the thread whose comment id is `parent`, if this page carries it.
///
/// This is what makes `GET …/comments/{comment_id}/replies` possible without inventing a token: the only
/// way to a reply page is the token a comment page rendered for that thread.
pub fn replies_token_for(payload: &Value, parent: &CommentId) -> Option<String> {
    for item in continuation_items(payload) {
        let Some(thread) = item.get("commentThreadRenderer") else {
            continue;
        };
        let id = thread
            .get("commentViewModel")
            .and_then(|view| view.get("commentViewModel"))
            .and_then(|view| view.get("commentId"))
            .and_then(Value::as_str)
            .or_else(|| {
                thread
                    .get("comment")
                    .and_then(|comment| comment.get("commentRenderer"))
                    .and_then(|renderer| renderer.get("commentId"))
                    .and_then(Value::as_str)
            });
        if id.is_some_and(|id| id == parent.as_str())
            && let Some(token) = replies_token(item)
        {
            return Some(token);
        }
    }
    None
}

/// The reply continuation token inside a thread's `replies.commentRepliesRenderer`.
pub fn replies_token(thread: &Value) -> Option<String> {
    let thread = thread.get("commentThreadRenderer")?;
    let items = thread
        .get("replies")?
        .get("commentRepliesRenderer")?
        .get("contents")?
        .as_array()?;
    items.iter().find_map(|item| continuation_token(item))
}

/// The page's own next-page token, when it rendered one.
pub fn next_page_token(payload: &Value) -> Option<String> {
    continuation_items(payload)
        .into_iter()
        .find_map(continuation_token)
}

/// Report counters for a payload that contains no `onResponseReceivedEndpoints` at all.
pub fn is_empty_response(payload: &Value) -> bool {
    continuation_items(payload).is_empty()
}

/// Renderers worth naming in a probe summary, for evidence gathering.
pub fn renderer_keys(payload: &Value) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in continuation_items(payload) {
        if let Some(key) = item.as_object().and_then(|map| map.keys().next()) {
            *counts.entry(key.clone()).or_insert(0) += 1;
        }
    }
    counts.into_iter().collect()
}
