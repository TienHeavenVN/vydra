//! Comment mapping: the `next` endpoint's comment section, in both renderer shapes it uses.
//!
//! Every fixture used here is `synthetic` (see `tests/fixtures/synthetic/comments_*.meta.json`): these tests
//! prove that the mapper reads the shapes Vydra *expects*, including the hostile ones, and prove nothing
//! about what upstream returns today. The genuinely captured pages live in `recorded/` once a probe run has
//! produced them, and `tests/comments_recorded.rs` is where a capture is asserted against.
//!
//! What each test is for:
//!
//! * the watch page either carries a comment section (with a relayed token) or does not — and "does not" is
//!   comments-disabled, not an empty list;
//! * a page mixes the classic `commentRenderer` shape with the current `commentViewModel` +
//!   `frameworkUpdates` entity join, and both map to the same domain type;
//! * a count that is absent stays `None`; an abbreviated count keeps its `raw` text and precision;
//! * a hostile row costs the row, not the page, and the cost is counted.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use serde_json::Value;
use time::OffsetDateTime;
use vydra_common::CommentId;
use vydra_domain::{CommentSort, Provenance, UnavailabilityReason};
use vydra_providers::innertube::comments::{
    comments_section_token, map_comments_page, map_replies_page, replies_token, replies_token_for,
    sort_options, sort_token,
};
use vydra_providers::parse::ParseReport;
use vydra_providers::ProviderError;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/synthetic/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{path} is not JSON: {err}"))
}

fn provenance() -> Provenance {
    Provenance {
        provider: "innertube-web".into(),
        endpoint: "next".into(),
        client: Some("web".into()),
        fetched_at: OffsetDateTime::now_utc(),
        cached: false,
        parse_warnings: 0,
        sources: vec![],
    }
}

fn parent() -> CommentId {
    CommentId::parse("UgxAAAAAAAAAAAAAAAAAAAA").expect("parent id")
}

// ---------------------------------------------------------------------------------------------
// Watch page: the comment section, or its absence
// ---------------------------------------------------------------------------------------------

#[test]
fn a_watch_page_with_a_comment_section_yields_its_relayed_token() {
    let page = fixture("comments_watch_page_with_section");
    let token = comments_section_token(&page).expect("watch page parses");
    assert_eq!(token.as_deref(), Some("PLACEHOLDER-WATCH-SECTION-TOKEN"));
}

/// The absence of the section is how upstream says "comments are off". It is not an empty page, and it must
/// not be reported as one.
#[test]
fn a_watch_page_without_a_comment_section_is_none_not_an_error() {
    let page = fixture("comments_watch_page_without_section");
    let token = comments_section_token(&page).expect("watch page parses");
    assert_eq!(token, None);
}

#[test]
fn a_payload_that_is_not_a_watch_page_is_a_parse_error() {
    let error = comments_section_token(&Value::Null).expect_err("must not invent a section");
    assert!(
        matches!(error, ProviderError::Parse { .. }),
        "expected a parse error, got {error:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// One page of comments, in both renderer shapes
// ---------------------------------------------------------------------------------------------

#[test]
fn a_page_maps_both_renderer_shapes_and_keeps_the_page_cursor() {
    let page = fixture("comments_page1_top");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());

    assert_eq!(list.items.len(), 3, "three threads, one of them duplicate-free");
    assert_eq!(list.sort, CommentSort::Top);
    assert_eq!(list.duplicates, 0);
    assert_eq!(list.next_token.as_deref(), Some("PLACEHOLDER-COMMENTS-PAGE2"));
    assert!(list.availability.is_available());
    assert_eq!(list.provenance.parse_warnings, report.count());
}

#[test]
fn the_entity_join_shape_carries_author_badges_counts_and_the_heart() {
    let page = fixture("comments_page1_top");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());
    let pinned = &list.items[0];

    assert_eq!(pinned.comment.id.as_str(), "UgxAAAAAAAAAAAAAAAAAAAA");
    assert_eq!(pinned.comment.text, "The pinned comment.");
    assert_eq!(pinned.comment.author.display_name, "Pinned Author");
    assert_eq!(
        pinned.comment.author.channel_id.as_ref().map(|id| id.as_str()),
        Some("UCaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(pinned.comment.author.is_creator, "isCreator is stated");
    assert!(pinned.comment.author.is_verified, "isVerified is stated");
    assert!(!pinned.comment.author.is_member, "no sponsor badge rendered");
    assert!(pinned.comment.is_pinned, "pinnedText is stated");
    assert!(pinned.comment.is_hearted, "toolbarStateKey says hearted");
    assert_eq!(
        pinned.comment.published_text, "2 days ago",
        "the localized label is preserved verbatim"
    );
    assert!(
        pinned.comment.published_at.is_none(),
        "a relative label is not an absolute time"
    );

    let likes = pinned.comment.like_count.as_ref().expect("like count");
    assert_eq!(likes.value, 1200, "1.2K is approximate");
    assert_eq!(likes.raw.as_deref(), Some("1.2K"));

    // The three reply facts are separate: the row claims fourteen, one came with the page, more exist.
    assert_eq!(pinned.total_reply_count, Some(14));
    assert_eq!(pinned.loaded_replies.len(), 1);
    assert!(pinned.has_more_replies);
    assert_eq!(pinned.unloaded_replies(), Some(13));
    let loaded = &pinned.loaded_replies[0];
    assert_eq!(loaded.id.as_str(), "UgxREPLYAAAAAAAAAAAAAAA");
    assert_eq!(
        loaded.parent_id.as_ref().map(|id| id.as_str()),
        Some("UgxAAAAAAAAAAAAAAAAAAAA"),
        "a loaded reply states its parent"
    );
    assert_eq!(loaded.published_text, "1 day ago (edited)");
}

#[test]
fn the_classic_renderer_shape_carries_its_channels_and_heart() {
    let page = fixture("comments_page1_top");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());
    let classic = &list.items[1];

    assert_eq!(classic.comment.id.as_str(), "UgxCCCCCCCCCCCCCCCCCCCC");
    assert_eq!(classic.comment.text, "A comment in the classic renderer shape.");
    assert_eq!(classic.comment.author.display_name, "Classic Renderer Author");
    assert_eq!(
        classic
            .comment
            .author
            .channel_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("UCcccccccccccccccccccccc")
    );
    assert_eq!(classic.comment.author.avatars.len(), 2, "both avatar sizes");
    assert!(!classic.comment.author.is_creator);
    assert!(classic.comment.is_hearted, "creatorHeart was rendered");
    assert!(!classic.comment.is_pinned);
    let likes = classic.comment.like_count.as_ref().expect("like count");
    assert_eq!(likes.value, 42);
    assert!(
        classic.comment.reply_count.is_none(),
        "the classic renderer states no numeric reply count and none is invented"
    );
    assert!(classic.loaded_replies.is_empty());
}

/// A row that states nothing is not a row that states zero.
#[test]
fn an_empty_toolbar_leaves_counts_absent_rather_than_zero() {
    let page = fixture("comments_page1_top");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());
    let third = &list.items[2];

    assert_eq!(third.comment.id.as_str(), "UgxDDDDDDDDDDDDDDDDDDDD");
    assert!(
        third.comment.like_count.is_none(),
        "an empty like label is not zero likes"
    );
    assert!(
        third.comment.reply_count.is_none(),
        "an empty reply label is not zero replies"
    );
    assert!(third.comment.availability.is_available());
}

// ---------------------------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------------------------

#[test]
fn the_sort_menu_yields_one_relayed_token_per_order() {
    let page = fixture("comments_page1_top");
    let options = sort_options(&page);

    assert_eq!(options.len(), 2);
    assert_eq!(options[0].title, "Top comments");
    assert_eq!(options[0].token, "PLACEHOLDER-SORT-TOP");
    assert!(options[0].selected);
    assert_eq!(options[1].title, "Newest first");
    assert_eq!(options[1].token, "PLACEHOLDER-SORT-NEWEST");
    assert!(!options[1].selected);

    assert_eq!(
        sort_token(&options, CommentSort::Top).expect("top"),
        "PLACEHOLDER-SORT-TOP"
    );
    assert_eq!(
        sort_token(&options, CommentSort::Newest).expect("newest"),
        "PLACEHOLDER-SORT-NEWEST"
    );
}

/// An order the page does not offer is unavailable, never approximated with the one it does offer.
#[test]
fn an_order_the_menu_does_not_offer_is_unsupported() {
    let page = fixture("comments_page1_newest");
    let mut options = sort_options(&page);
    options.truncate(1);

    let error = sort_token(&options, CommentSort::Newest).expect_err("must refuse");
    match error {
        ProviderError::Unsupported { capability } => {
            assert_eq!(capability, "comments_sort:newest");
        }
        other => panic!("expected unsupported, got {other:?}"),
    }
}

#[test]
fn the_newest_page_selection_is_read_from_the_menu() {
    let page = fixture("comments_page1_newest");
    let options = sort_options(&page);
    assert_eq!(options.len(), 2);
    assert!(!options[0].selected);
    assert!(options[1].selected);
}

// ---------------------------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------------------------

#[test]
fn a_thread_renders_its_reply_continuation_and_the_page_finds_it_by_parent() {
    let page = fixture("comments_page1_top");
    assert_eq!(
        replies_token_for(&page, &parent()).as_deref(),
        Some("PLACEHOLDER-REPLIES-THREAD1")
    );
    let other = CommentId::parse("UgxDDDDDDDDDDDDDDDDDDDD").expect("id");
    assert_eq!(
        replies_token_for(&page, &other),
        None,
        "a thread without a reply continuation yields no token; none is constructed"
    );

    // The same token is reachable from the thread node itself.
    let thread = &page["onResponseReceivedEndpoints"][1]["appendContinuationItemsAction"]
        ["continuationItems"][0];
    assert_eq!(
        replies_token(thread).as_deref(),
        Some("PLACEHOLDER-REPLIES-THREAD1")
    );
}

#[test]
fn a_reply_page_maps_both_reply_shapes_and_stamps_the_parent() {
    let page = fixture("comments_replies_page1");
    let mut report = ParseReport::new();
    let list = map_replies_page(&page, &parent(), 20, &mut report, provenance());

    assert_eq!(list.parent_id, parent());
    assert_eq!(list.items.len(), 2);
    assert_eq!(list.next_token.as_deref(), Some("PLACEHOLDER-REPLIES-PAGE2"));
    assert!(
        list.total_reply_count.is_none(),
        "a reply page does not restate the thread total; the parent row is the only statement of it"
    );
    for reply in &list.items {
        assert_eq!(reply.parent_id.as_ref(), Some(&list.parent_id));
    }
    assert_eq!(list.items[0].text, "A loaded reply.");
    assert_eq!(list.items[1].text, "A reply in the classic renderer shape.");
    assert!(
        list.items[1].author.is_creator,
        "authorIsChannelOwner is the classic creator flag"
    );
}

// ---------------------------------------------------------------------------------------------
// Hostile rows
// ---------------------------------------------------------------------------------------------

/// A bad row costs the row. The page keeps its good rows, its cursor and its position.
#[test]
fn hostile_rows_are_counted_and_the_good_rows_survive() {
    let page = fixture("comments_page1_malformed_rows");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());

    assert_eq!(list.items.len(), 2, "the two valid, distinct threads");
    assert_eq!(list.duplicates, 1, "the repeated comment id is counted");
    assert_eq!(
        list.next_token.as_deref(),
        Some("PLACEHOLDER-COMMENTS-PAGE2"),
        "the first cursor wins; the second is reported, not used"
    );

    let messages = report.messages().join("\n");
    assert!(
        messages.contains("skipped_item:comment_view_model_without_id"),
        "an entity with no comment id is skipped and counted: {messages}"
    );
    assert!(
        messages.contains("skipped_item:comment_view_model_id_invalid"),
        "an invalid comment id is skipped and counted: {messages}"
    );
    assert!(
        messages.contains("unmapped_content:someUnknownRenderer"),
        "an unknown renderer is named: {messages}"
    );
    assert!(
        messages.contains("unmapped_content:commentThreadRenderer_without_body"),
        "a thread with no body is named: {messages}"
    );
    assert!(
        messages.contains("comments_extra_continuation"),
        "a second page cursor is reported: {messages}"
    );
    assert!(report.content_problems() >= 4, "{}", report.count());
}

/// The two decisions that are easiest to get wrong: a name is not an id, and a limit truncates the page
/// without silently discarding the cursor.
#[test]
fn an_author_name_is_never_promoted_to_an_identifier() {
    let page: Value = serde_json::from_str(
        r#"{
          "onResponseReceivedEndpoints": [{
            "appendContinuationItemsAction": {"continuationItems": [
              {"commentThreadRenderer": {"commentViewModel": {
                "commentId": "UgxEEEEEEEEEEEEEEEEEEEE", "commentKey": "ENTITY-NAME-ONLY"
              }}}
            ]}
          }],
          "frameworkUpdates": {"entityBatchUpdate": {"mutations": [{
            "entityKey": "ENTITY-NAME-ONLY",
            "payload": {"commentEntityPayload": {
              "properties": {"commentId": "UgxEEEEEEEEEEEEEEEEEEEE", "content": {"content": "no id here"}},
              "author": {"displayName": "A Handle Looking Name"}
            }}
          }]}}
        }"#,
    )
    .expect("inline synthetic page");

    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());
    assert_eq!(list.items.len(), 1);
    let comment = &list.items[0].comment;
    assert_eq!(comment.author.display_name, "A Handle Looking Name");
    assert!(
        comment.author.channel_id.is_none(),
        "no channel id was stated, so none exists — nothing is derived from the name"
    );
}

#[test]
fn the_page_limit_truncates_and_says_so() {
    let page = fixture("comments_page1_top");
    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 2, &mut report, provenance());
    assert_eq!(list.items.len(), 2);
    assert!(
        report.messages().iter().any(|m| m == "page_truncated"),
        "truncation is reported: {:?}",
        report.messages()
    );
}

/// A row rendered with no text and no author is what a deleted comment looks like; it keeps its place.
#[test]
fn a_row_with_no_text_and_no_author_is_marked_deleted_not_dropped() {
    let page: Value = serde_json::from_str(
        r#"{
          "onResponseReceivedEndpoints": [{
            "appendContinuationItemsAction": {"continuationItems": [
              {"commentThreadRenderer": {"comment": {"commentRenderer": {
                "commentId": "UgxFFFFEEEEEEEEEEEEEEEE"
              }}}}
            ]}
          }]
        }"#,
    )
    .expect("inline synthetic page");

    let mut report = ParseReport::new();
    let list = map_comments_page(&page, CommentSort::Top, 20, &mut report, provenance());
    assert_eq!(list.items.len(), 1, "the row keeps its position");
    let comment = &list.items[0].comment;
    assert!(comment.text.is_empty());
    assert_eq!(
        comment.availability,
        vydra_domain::Availability::Unavailable {
            reason: UnavailabilityReason::Deleted
        }
    );
    assert!(
        report.messages().iter().any(|m| m == "comment_author_missing"),
        "the lost author is reported: {:?}",
        report.messages()
    );
}
