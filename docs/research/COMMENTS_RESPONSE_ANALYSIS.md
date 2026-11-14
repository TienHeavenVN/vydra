# Comments response analysis

What a comment page actually contains, which of its shapes Vydra reads, and what it refuses to infer.

Normative code: `crates/vydra-providers/src/innertube/comments.rs` (parsing and mapping),
`crates/vydra-domain/src/comment.rs` (the documents), `crates/vydra-providers/src/parse.rs` (report and text
helpers). Request side: `docs/research/COMMENTS_REQUEST_CONTRACT.md`. Fixtures:
`docs/research/COMMENTS_FIXTURE_PROVENANCE.md`. Verification boundaries: `docs/reports/PHASE_7_REPORT.md` §7.

## 1. The page envelope

A comment page is a `next` response. The body of the page is **not** at the top level:

```
onResponseReceivedEndpoints[last]           ← the *last* endpoint carries the page body
  .reloadContinuationItemsCommand.continuationItems[]   (initial page)
  | .appendContinuationItemsAction.continuationItems[]   (subsequent pages, replies)
```

The *first* endpoint carries the header on the initial response (which is where the sort menu lives); some responses
have a single endpoint. `comments::continuation_items` therefore walks `onResponseReceivedEndpoints` backwards and
returns the first endpoint that carries a non-empty `continuationItems` array.

Consequences:

* a payload with no `onResponseReceivedEndpoints` yields **no** items and no token; `comments::is_empty_response`
  says so explicitly rather than leaving a caller to infer it from an empty vector;
* the *position* of a renderer inside `continuationItems` is meaningful (0 is the first comment of the page), so the
  mapper never sorts or reorders;
* a second `continuationItemRenderer` on one page is not a second page. The first is used, the second is counted
  (`comments_extra_continuation`) and dropped.

## 2. The two comment shapes, both live

A comment row arrives in one of two shapes, and the same page may contain both — the migration between them is
partial, per surface and per row.

**A. The current shape: a view model plus an entity join.**

```jsonc
{"commentThreadRenderer": {"commentViewModel": {
    "commentId": "Ugx…", "commentKey": "Eg…", "toolbarStateKey": "Eg…", "pinnedText": "Pinned by …"
}, "replies": {"commentRepliesRenderer": {"contents": [ … ]}}}}
```

The view model carries **ids and keys only**. Everything a client displays lives in a sibling entity batch update:

```jsonc
"frameworkUpdates": {"entityBatchUpdate": {"mutations": [
  {"entityKey": "Eg…", "payload": {"commentEntityPayload": {
      "properties": {"commentId": "Ugx…", "content": {"content": "…"}, "publishedTime": "2 days ago"},
      "author": {"channelId": "UC…", "displayName": "…", "isCreator": true, "isVerified": true,
                 "avatarThumbnailUrl": "https://…"},
      "toolbar": {"likeCountNotliked": "1.2K", "replyCount": "14", "likeCountLiked": ""}
  }}},
  {"entityKey": "Eg…", "payload": {"engagementToolbarStateEntityPayload": {"heartState": "TOOLBAR_HEART_STATE_HEARTED"}}}
]}}
```

The join is `commentViewModel.commentKey → mutation.entityKey → payload.commentEntityPayload`, and the heart is
`commentViewModel.toolbarStateKey → mutation.entityKey → payload.engagementToolbarStateEntityPayload.heartState`.
This is exactly what `LuanRT/YouTube.js` does (`CommentView.ts` exposes the keys; `CommentEntityPayload` supplies the
content) and what `NewPipeExtractor`'s `YoutubeCommentsEUVMInfoItemExtractor` reads (`properties.content.content`,
`author.displayName`, `toolbar.likeCountNotliked`, `toolbar.replyCount`, `heartState`).

**B. The classic shape: everything in one renderer.**

```jsonc
{"commentThreadRenderer": {"comment": {"commentRenderer": {
    "commentId": "Ugx…",
    "authorText": {"simpleText": "…"},
    "authorEndpoint": {"browseEndpoint": {"browseId": "UC…"}},
    "authorThumbnail": {"thumbnails": [ … ]},
    "contentText": {"runs": [{"text": "…"}]},
    "publishedTimeText": {"runs": [{"text": "3 days ago"}]},
    "voteCount": {"simpleText": "42"},
    "authorIsChannelOwner": false,
    "authorCommentBadge": { … }, "sponsorCommentBadge": { … },
    "actionButtons": {"commentActionButtonsRenderer": {"creatorHeart": { … }}}
}}}}
```

Rows in a reply page are **bare**: `commentViewModel` or `commentRenderer` without a surrounding
`commentThreadRenderer`. `comment_from_row` handles all four combinations (thread+view model, thread+renderer, bare
view model, bare renderer) and reports any other key by name.

## 3. Field by field: what is read, and what is refused

| Domain field | View-model shape | Classic shape | Refusal |
| --- | --- | --- | --- |
| `id` | `properties.commentId`, else `commentViewModel.commentId` | `commentRenderer.commentId` | must pass `CommentId::parse` (8–200 chars, `[A-Za-z0-9_-]`); a failing id drops the row and is counted |
| `text` | `properties.content.content` (attributed description), else `content` text | `contentText` | an empty body is kept and reported (`comment_text_empty`): a deleted comment renders as one |
| `author.display_name` | `author.displayName` (required) | `authorText` | a view-model row with no display name is dropped (`missing_field`) |
| `author.channel_id` | `author.channelId`, else `channelCommand.innertubeCommand.browseEndpoint.browseId` | `authorEndpoint.browseEndpoint.browseId` | must pass `ChannelId::parse`; otherwise `None` + `comment_author_id_invalid`. **Never derived from the name** |
| `author.avatars` | `avatar.image.sources`, else `author.avatarThumbnailUrl` | `authorThumbnail.thumbnails` | unreadable list → empty list, comment kept |
| `author.is_creator` | `author.isCreator` | `authorIsChannelOwner` | absent means `false`, which is what the field means: the page did not mark the author as the owner |
| `author.is_verified` | `isVerified` or `isArtist` | `authorCommentBadge` present | absent means `false` |
| `author.is_member` | `sponsorBadgeUrl` present | `sponsorCommentBadge` present | absent means `false` |
| `published_text` | `properties.publishedTime` | `publishedTimeText` | kept verbatim, including the `"(edited)"` suffix and any localization |
| `published_at` | — | — | **always `None`**: the page renders a relative label, and turning "2 days ago" into an instant would be arithmetic on a guess |
| `like_count` | `toolbar.likeCountNotliked` | `voteCount` | `None` when absent or empty; `Count::Approximate` with `raw` when abbreviated (`1.2K`) |
| `reply_count` | `toolbar.replyCount` | — | `None` when absent or empty. The classic renderer states only a localized "View N replies" label, which is not parsed |
| `is_pinned` | `commentViewModel.pinnedText` non-empty | `pinnedCommentBadge` present | `false` when absent |
| `is_hearted` | `toolbarStateKey` → `heartState == TOOLBAR_HEART_STATE_HEARTED` | `actionButtons.commentActionButtonsRenderer.creatorHeart` present | `false` when absent |
| `parent_id` | set to the thread's own id on loaded replies; the cursor's parent on a reply page | same | never invented for a top-level comment |
| `availability` | `Unavailable{Deleted}` when text **and** channel id are both absent | `Unavailable{Deleted}` when display name and text are both empty | otherwise `Available`; `Unknown` is never produced here because the row itself is the statement that the comment exists |

Rules that shaped that table:

1. **A missing count is not a zero.** `like_count: None` and `like_count: Some(0)` are different facts, and only the
   page can say which. Empty strings in the toolbar (`"likeCountNotliked": ""`) are treated as absent.
2. **A display name is not an identifier.** No id is derived from a name, and no name is parsed as a handle.
3. **A relative label is not a timestamp.** `published_text` keeps what was rendered; `published_at` exists for
   providers that state an instant and is `None` here.
4. **A partial comment is still a comment.** A malformed avatar list costs the avatars; a malformed id costs the row.
5. **Deleted is stated, not assumed.** A row with no text and no author is marked `Deleted` and keeps its position:
   dropping it would renumber the page.

## 4. Threads, replies and the three reply facts

```jsonc
{"commentThreadRenderer": {
   "commentViewModel": { … },                                  ← the top-level comment
   "replies": {"commentRepliesRenderer": {"contents": [
       {"commentViewModel": { … }},                            ← a reply that came with the page
       {"continuationItemRenderer": { … }}                     ← "more replies exist"
   ]}}
}}
```

* `total_reply_count` = the row's `toolbar.replyCount` (view-model shape only), i.e. **what the page claimed**;
* `loaded_replies` = the replies that came in `replies.commentRepliesRenderer.contents`, mapped by the same code as
  top-level comments, with `parent_id` set to the thread's own id;
* `has_more_replies` = that array contained a continuation item;
* `CommentThread::unloaded_replies()` = `total - loaded`, `None` when the total is unknown or already satisfied —
  **never `0` because nothing was loaded**.

A reply page is a `ReplyList` and never restates the total: `total_reply_count` is `None` there, because the parent
row is the only statement of it and a second, differently-scoped number would be a second source of truth. Every row
on a reply page is stamped with the cursor's parent id, and the probe reports how many rows disagreed
(`rows_with_another_parent`), which should be zero.

## 5. Structure that is not a comment

A comment page is mostly chrome. `comments::renderer_keys` and the probe's shape inventory name what a page
carries; the mapper treats a row's key as either content or structural, and anything else is *named*, not silently
skipped:

* structural keys (no content, no mapping attempted): `commentRepliesRenderer`, `commentThreadRenderer` (handled),
  `commentsHeaderRenderer`, `commentsSimpleboxRenderer`, `commentSimpleboxRenderer`, `commentActionButtonsRenderer`,
  `commentDialogRenderer`, `commentsEntryPointHeaderRenderer`, `sectionListRenderer`, `itemSectionRenderer`,
  `continuationItemRenderer`;
* `continuationItemRenderer` inside `continuationItems` is the page cursor and is *used*, not counted;
* every other key is `report.unmapped_result(key)` — which `Strictness::Strict` fails on, so a new content shape in
  the item array cannot pass unnoticed.

This is why the module walks the item array itself instead of reusing `parse::collect_renderers`: a whole-page walk
counts hundreds of `buttonViewModel` / `listItemViewModel` / `chipViewModel` keys and would drown the report.

## 6. Duplicates, limits and cursor sanity

* **Duplicates are counted, not silently merged.** A comment id seen twice in one page is dropped the second time and
  added to `CommentList::duplicates` / `ReplyList::duplicates`. A duplicate row is upstream repeating itself, which
  is worth reporting and not worth serving twice.
* **A limit truncates the page, not the cursor.** `limit` is applied after mapping, the truncation is reported
  (`page_truncated`), and the page's own next token is still returned: the client can page on.
* **A token equal to the request token is dropped** by the engine (`*_next_token = None`, warning counted): keeping
  it would let a client loop forever.
* **Tokens are opaque.** The only checks are non-empty and ≤ 4096 bytes (`comments::continuation_token`) before
  being handed to `NextRequest::continuation`, which applies its own bound and character-set check.

## 7. What the parser does not do

* It does not read `commentsHeaderRenderer.commentsCount` as a total-comment count for the video. That number is
  upstream's own statement about the section, it is localized, and it is not the sum of anything Vydra serves.
* It does not parse "View 14 replies" labels into counts.
* It does not read reply rows as threads, or thread rows as replies: the two map to different domain shapes.
* It does not interpret `content` markup. `properties.content.content` is an attributed description; the text is
  taken as rendered, with no link, bold or mention expansion.
* It does not follow `onResponseReceivedActions` (the other `next` envelope key). No reference implementation reads
  comment pages from it, and the fixtures do not exercise it; a page that used only that key would be an empty
  comment page and would be reported as such.
