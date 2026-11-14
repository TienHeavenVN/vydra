# Comments request contract

What Vydra sends when it asks for comments, why each request has that shape, and which orders it refuses to
offer.

Normative code: `crates/vydra-providers/src/innertube/request.rs` (`NextRequest`),
`crates/vydra-providers/src/innertube/provider.rs` (`comments`, `comment_replies`, `comments_section_token`,
`find_reply_token`), `crates/vydra-providers/src/innertube/comments.rs` (token discovery, sort menu).
Response side: `docs/research/COMMENTS_RESPONSE_ANALYSIS.md`. Fixtures: `docs/research/COMMENTS_FIXTURE_PROVENANCE.md`.
Verification boundaries: `docs/reports/PHASE_7_REPORT.md` §7.

## 1. Why a comment page is two requests, not one

The comment section has no addressable endpoint. It is reached through a **continuation token**, and that token
exists only in a page that already rendered the section:

```
POST /youtubei/v1/next {context, videoId}          → watch page
   └─ contents.…itemSectionRenderer{targetId: "comments-section"}.contents[].continuationItemRenderer
        .continuationEndpoint.continuationCommand.token     ← the section token

POST /youtubei/v1/next {context, continuation: token} → comment page 1
   └─ onResponseReceivedEndpoints[last].{reloadContinuationItemsCommand|appendContinuationItemsAction}
        .continuationItems[]                                ← header, threads, next token
```

Every reference implementation does exactly this. `NewPipeExtractor`
(`YoutubeCommentsExtractor.getComments`) walks `contents.twoColumnWatchNextResults.results.results.contents`
looking for `itemSectionRenderer.targetId == "comments-section"`, reads
`continuationItemRenderer.continuationEndpoint.continuationCommand.token`, and then issues the second request.
`LuanRT/YouTube.js` (`src/parser/youtube/Comments.ts`) does the same through its navigation helpers, and reads the
section token from the watch page's `CommentsSection` endpoint.

Consequences Vydra accepts:

* **page 1 costs two upstream calls.** The engine caches the first page for `cache.comments_ttl_secs`, which is
  what keeps that discovery cost from being paid per request;
* **page *n* costs one call** — a cursor is a base64 continuation token and nothing else;
* **there is no way to ask for comments of a video that renders no section.** That absence is the signal (see §4).

## 2. The two bodies

`NextRequest` is the only type that can produce a `next` body, and it can produce exactly two shapes:

| Body | Constructor | Used for |
| --- | --- | --- |
| `{context, videoId}` | `NextRequest::comments_section(context, id)` | the watch page, whose section token is the only entry point |
| `{context, continuation}` | `NextRequest::continuation(context, token)` | every comment page, every reply page, every order |

There is no `params` field, and the type cannot express `videoId` **and** `continuation` at once. That is a
deliberate restriction, not a missing feature:

* no reference implementation sends `params` for comments, so a `params` field would be an encoding Vydra invented;
* sending both keys would be a body no client sends, and a page that answers it would teach Vydra nothing about
  the shape the real clients use.

`NextRequest::continuation` validates the token before any request is made: non-empty, ≤ 8 KiB, and free of
whitespace and control characters. A token that fails is `ProviderError::InvalidRequest { field: "continuation",
issue: "invalid_token" }` — a `400` at the edge, with **no upstream request** behind it. The bound is not a
correctness claim about upstream tokens; it is a refusal to relay a blob that could not be a token.

The proof that these are the bodies Vydra sends is `tests/request_contract.rs`:

* `comments_ask_for_the_watch_page_then_post_only_the_relayed_token` — replays a transport, asserts the first
  `next` body is exactly `["context", "videoId"]` and the second is exactly `["context", "continuation"]`, that the
  continuation is the token the watch page carried, that `videoId` is not repeated, and that `params` is absent;
* `a_comment_cursor_page_is_one_continuation_call` — a cursor page is one call;
* `a_continuation_that_is_not_a_token_is_refused_before_the_wire` — four malformed tokens, zero upstream calls.

Each test asserts the request only. None of them claims anything about how upstream answers; that is what the
response analysis and the fixture tests are for.

## 3. Sorting: two orders, both relayed

Upstream does not take a sort *parameter*. The comment page renders a menu, and each entry of that menu carries a
continuation token that means "this same section, in this order":

```
commentsHeaderRenderer.sortMenu.sortFilterSubMenuRenderer.subMenuItems[] = [
  {title: "Top comments",  selected: true,  serviceEndpoint.continuationCommand.token: <token A>},
  {title: "Newest first",  selected: false, serviceEndpoint.continuationCommand.token: <token B>}
]
```

Vydra's rules:

1. **Only `top` and `newest` are offered**, because those are the two entries the menu carries in every reference
   implementation and in the probe. Any other value is `400` (`sort: unsupported`) — see
   `CommentsQuery::sort`.
2. **The order's meaning is positional**, which is what the references rely on: entry 0 is "Top comments", entry 1
   is "Newest first". A menu with one entry therefore offers one order.
3. **The token is relayed, never constructed.** `sort_token(options, sort)` returns entry *n*'s token or
   `Unsupported { capability: "comments_sort:<sort>" }`. Vydra has no table of sort tokens, no `params` encoding for
   an order, and no way to ask for an order the page did not render.
4. **`top` is the default**, because it is what a page selects when nobody says otherwise, and the page says so
   itself (`selected: true`).
5. **A response cannot state its own order.** `CommentList::sort` therefore records the order Vydra *requested*.
   If the page hands back a different order's body, Vydra cannot detect that, and the field does not pretend
   otherwise.

Behaviour when the requested order is not the one the first page landed in — this is a real case, since the section
token opens the page in whatever order the viewer last used:

| Requested | First page's selection | Upstream calls |
| --- | --- | --- |
| `top` | `top` selected | 2 (watch page + page) |
| `newest` | `top` selected | 3 (watch page + page, then the menu's `newest` token) |
| any | menu offers one entry only | 2 if that entry is the requested order, otherwise `Unsupported` |

Not modelled, and deliberately absent: "newest replies", "top replies", any per-language order name, and the
comment-count filter chips some surfaces render. None of them has a verified request contract, so none is offered.

## 4. Comments turned off

A watch page whose comment section was disabled renders **no** `itemSectionRenderer` with
`targetId == "comments-section"` (and none with `sectionIdentifier == "comment-item-section"`). The section is
absent, not empty. `comments_section_token` returns:

| Watch page | Result | API |
| --- | --- | --- |
| section present with a token | `Ok(Some(token))` | normal comment page |
| no comment section | `Ok(None)` | `409 content_unavailable`, `details[0].issue: comments_disabled` |
| not a watch page at all (no `contents`) | `Err(Parse)` | `502 provider_parse` (not a 409) |

The distinction matters because the three cases have three different meanings for a client: "read the page and it
was empty", "the uploader turned comments off", and "Vydra could not read the page". Only the middle one is a
content fact.

A video that does not exist is a fourth case and is *not* handled here: the watch page for a bad id renders
neither, and `GET /videos/{id}/comments` answers `404` because the upstream `next` call fails with a content fact
the provider already classifies. The probe records what an unresolvable id actually returns
(`missing_video_watch`) rather than assuming it looks like a disabled section.

## 5. Replies: a token that must be found, not built

A reply page is addressed by the same kind of token, and that token lives inside the comment page that rendered the
thread:

```
commentThreadRenderer.replies.commentRepliesRenderer.contents[] = [
  {commentRenderer | commentViewModel …},      ← replies already loaded
  {continuationItemRenderer.continuationEndpoint.continuationCommand.token: <reply token>}
]
```

So `GET /videos/{id}/comments/{comment_id}/replies` with no cursor performs a bounded search:

1. resolve the section token (watch page);
2. fetch comment pages one at a time, up to `REPLY_SCAN_PAGES = 3`, looking for a thread whose comment id equals
   the requested parent (`comments::replies_token_for`);
3. if found, relay that thread's reply token; if not, `404 not_found` with the message
   `comment thread not found on the first 3 comment pages of <video id>`.

Vydra never derives a reply token from a comment id. That encoding is undocumented, unverified, and a wrong token
would page some other thread — an error a client could not detect. The page scan is a cost bound, not a correctness
knob: every scanned page is a real request, and the answer "not on the first three pages" is honest.

The three pages are scanned in the **default** order. A thread that only appears in `newest` order may therefore be
reported `not_found` — a limitation recorded in `PHASE_7_REPORT.md` §7 rather than papered over.

## 6. What Vydra never sends

| Tempting request | Why not |
| --- | --- |
| `params` for a comment section or an order | no reference implementation sends one; inventing the encoding is a guess |
| `next` with `videoId` **and** `continuation` | no client sends both; a page answering it proves nothing |
| a reply token derived from a comment id | undocumented encoding; a wrong token silently pages another thread |
| a `POST` that creates, edits, deletes or likes | out of scope: Vydra's engine is read-only (§ `docs/security/PROVIDER_ENGINE_AUDIT.md`) |
| a signed-in request, a consent cookie, or a bot-check bypass | no session is created for comments, and a refusal is reported as a refusal |
| an enumeration of videos to find one with comments | out of scope, and indistinguishable from abuse |
| more than three comment pages to find a thread | bounded cost; the answer "not found" is acceptable, unbounded scanning is not |
