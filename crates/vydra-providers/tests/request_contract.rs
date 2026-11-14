//! Request-shape contract tests.
//!
//! These pin **what Vydra sends** — endpoint, query string, headers and JSON body — by replaying a
//! transport and inspecting the recorded `TransportRequest`s. They deliberately assert nothing
//! about how upstream answers: no request left the process, so any claim about the *response* side
//! belongs to the fixture tests (`recorded_fixture.rs`) instead.
//!
//! The shapes are backed by the reference implementations collected in
//! `docs/research/INNERTUBE_REQUEST_CONTRACTS.md`; each test names the reference it mirrors.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use vydra_common::VideoId;
use vydra_domain::CommentSort;
use vydra_errors::{ProviderError, UnavailabilityReason};
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::{
    HttpMethod, Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport,
    SearchFilters, SearchKind, Strictness, TransportRequest, TransportResponse,
};

/// Bootstrap document shaped like `https://www.youtube.com/sw.js_data` (`[[{...}], "AIza..."]`).
///
/// The visitor id and key are obvious placeholders — they are shaped correctly, not real, and no
/// test asserts anything about their validity upstream.
const BOOTSTRAP: &str = r#"[[{"visitorData":"VISITOR-FIXTURE"}],"AIzaSYFIXTUREKEY0123456789"]"#;

const SEARCH_BODY: &str = r#"{"contents":[]}"#;
const PLAYER_BODY: &str =
    r#"{"playabilityStatus":{"status":"OK"},"videoDetails":{"videoId":"9Umm2MAKsMc","title":"t"}}"#;

fn context(strictness: Strictness) -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "contract-test".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness,
    }
}

fn replay(bootstrap: &str, payload: &str) -> Arc<ReplayTransport> {
    replay_many(bootstrap, &[payload])
}

/// One bootstrap document followed by one response per endpoint call.
///
/// The session is cached across calls but every endpoint call still consumes a response, so a test
/// that fetches twice needs two payloads — a queue that runs dry fails loudly with a transport error
/// rather than silently reusing the previous body.
fn replay_many(bootstrap: &str, payloads: &[&str]) -> Arc<ReplayTransport> {
    let mut queued = vec![Ok(TransportResponse {
        status: 200,
        headers: vec![],
        body: Bytes::from(bootstrap.to_owned()),
        elapsed: Duration::from_millis(1),
    })];
    queued.extend(payloads.iter().map(|payload| {
        Ok(TransportResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Bytes::from((*payload).to_owned()),
            elapsed: Duration::from_millis(3),
        })
    }));
    Arc::new(ReplayTransport::new(queued))
}

fn innertube(transport: Arc<ReplayTransport>) -> InnertubeProvider {
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    InnertubeProvider::new(config, transport)
}

fn header<'a>(request: &'a TransportRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn body_of(request: &TransportRequest) -> Value {
    let body = request.body.as_ref().expect("request body");
    serde_json::from_slice(body).expect("request body is JSON")
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("body is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort_unstable();
    keys
}

fn search_request(cursor: Option<&str>) -> ProviderRequest {
    ProviderRequest::Search {
        query: "captain jack".into(),
        kind: SearchKind::All,
        filters: SearchFilters::default(),
        cursor: cursor.map(str::to_owned),
        hl: "en".into(),
        gl: "US".into(),
        limit: 20,
    }
}

// --------------------------------------------------------------------------------------------
// Search
// --------------------------------------------------------------------------------------------

/// Search posts `{context, query}` to `POST /youtubei/v1/search` and nothing else.
///
/// References: `MediaServiceCore/.../search/v1/SearchApi.java:18` (`SEARCH_URL`) and
/// `.../search/SearchApiHelper.java:6-8` (body `{"query": ...}`); yt-dlp
/// `_base.py:852-866` builds `{'context': ..., **query}` and puts the key in the query string.
#[tokio::test]
async fn search_posts_context_and_query_to_the_search_endpoint() {
    let transport = replay(BOOTSTRAP, SEARCH_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(search_request(None), &context(Strictness::Lenient))
        .await
        .expect("search response");

    let calls = transport.calls();
    assert_eq!(calls.len(), 2, "one bootstrap call and one search call");
    let search = &calls[1];

    assert_eq!(search.method, HttpMethod::Post);
    assert_eq!(
        search.url.as_str(),
        // `prettyPrint=false` is what MediaServiceCore `HTTPClient.kt:44` sets; the key is a query
        // parameter, never a header (yt-dlp `_base.py:862-866`).
        "https://www.youtube.com/youtubei/v1/search?prettyPrint=false&key=AIzaSYFIXTUREKEY0123456789"
    );

    let body = body_of(search);
    assert_eq!(sorted_keys(&body), vec!["context", "query"]);
    assert_eq!(body["query"], "captain jack");

    let client = &body["context"]["client"];
    assert_eq!(client["clientName"], "WEB");
    assert_eq!(client["hl"], "en");
    assert_eq!(client["gl"], "US");
    assert_eq!(client["visitorData"], "VISITOR-FIXTURE");
    assert_eq!(client["originalUrl"], "https://www.youtube.com");
    assert_eq!(body["context"]["user"]["lockedSafetyMode"], false);
    assert_eq!(body["context"]["request"]["useSsl"], true);

    // Headers: `X-YouTube-Client-Name` / `-Version` / `X-Goog-Visitor-Id` + a client-matching UA.
    assert_eq!(header(search, "content-type"), Some("application/json"));
    assert_eq!(header(search, "x-youtube-client-name"), Some("1"));
    assert_eq!(
        header(search, "x-youtube-client-version"),
        Some(provider.profile().version.as_str())
    );
    assert_eq!(header(search, "origin"), Some("https://www.youtube.com"));
    assert_eq!(header(search, "x-goog-visitor-id"), Some("VISITOR-FIXTURE"));
    let user_agent = header(search, "user-agent").expect("user agent");
    assert!(
        user_agent.contains("Chrome/"),
        "the UA must match the WEB client profile: {user_agent}"
    );
}

/// A continuation page carries the token instead of the query — the reference implementations do
/// not repeat the query (`MediaServiceCore` `SearchApiHelper.CONTINUATION_SEARCH`, yt-dlp
/// `_base.py:1056` sets only `continuation`).
#[tokio::test]
async fn continuation_posts_only_the_token() {
    let transport = replay(BOOTSTRAP, SEARCH_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(
            search_request(Some("CBQSCBg")),
            &context(Strictness::Lenient),
        )
        .await
        .expect("continuation response");

    let calls = transport.calls();
    let body = body_of(&calls[1]);
    assert_eq!(sorted_keys(&body), vec!["context", "continuation"]);
    assert_eq!(body["continuation"], "CBQSCBg");
    assert!(
        body.get("query").is_none(),
        "continuation pages must not repeat the query"
    );
    assert!(
        body.get("params").is_none(),
        "filters are bound to the first page only"
    );
}

// --------------------------------------------------------------------------------------------
// Player
// --------------------------------------------------------------------------------------------

/// Player posts `{context, videoId, contentCheckOk, racyCheckOk}` — the flags are what the web
/// client sends so that age-restricted metadata is not silently dropped by the *request*.
#[tokio::test]
async fn player_posts_the_video_id_with_the_check_flags() {
    let transport = replay(BOOTSTRAP, PLAYER_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(
            ProviderRequest::Video {
                id: VideoId::parse("9Umm2MAKsMc").expect("valid id"),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(Strictness::Lenient),
        )
        .await
        .expect("player response");

    let calls = transport.calls();
    let player = &calls[1];
    assert_eq!(player.method, HttpMethod::Post);
    assert_eq!(
        player.url.as_str(),
        "https://www.youtube.com/youtubei/v1/player?prettyPrint=false&key=AIzaSYFIXTUREKEY0123456789"
    );

    let body = body_of(player);
    assert_eq!(
        sorted_keys(&body),
        vec!["contentCheckOk", "context", "racyCheckOk", "videoId"]
    );
    assert_eq!(body["videoId"], "9Umm2MAKsMc");
    assert_eq!(body["contentCheckOk"], true);
    assert_eq!(body["racyCheckOk"], true);
    assert_eq!(body["context"]["client"]["clientName"], "WEB");
}

/// The player body is byte-exact: field order included, because a reordered body is a changed
/// contract even when the JSON is equivalent.
#[tokio::test]
async fn player_body_is_byte_exact() {
    let transport = replay(BOOTSTRAP, PLAYER_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(
            ProviderRequest::Video {
                id: VideoId::parse("9Umm2MAKsMc").expect("valid id"),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(Strictness::Lenient),
        )
        .await
        .expect("player response");

    let calls = transport.calls();
    let sent = calls[1].body.as_deref().expect("player request body");
    let sent = std::str::from_utf8(sent).expect("body is utf-8");

    // Wire order is *alphabetical*, not struct order: the typed body is converted to
    // `serde_json::Value` before encoding (`provider.rs::serialize_body`), and `serde_json::Map` is a
    // `BTreeMap` without the `preserve_order` feature. The bytes are still deterministic, which is
    // what makes this assertion meaningful; the docs say "key-sorted" and this test is the proof.
    let expected = format!(
        r#"{{"contentCheckOk":true,"context":{{"client":{{"clientName":"WEB","clientVersion":"{}","gl":"US","hl":"en","originalUrl":"https://www.youtube.com","platform":"DESKTOP","visitorData":"VISITOR-FIXTURE"}},"request":{{"internalExperimentFlags":[],"useSsl":true}},"user":{{"lockedSafetyMode":false}}}},"racyCheckOk":true,"videoId":"9Umm2MAKsMc"}}"#,
        provider.profile().version
    );
    assert_eq!(sent, expected);
}

/// A player request must not inherit anything from search: no query, no filters, no cursor.
#[tokio::test]
async fn player_request_carries_no_search_only_fields() {
    let transport = replay(BOOTSTRAP, PLAYER_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(
            ProviderRequest::Video {
                id: VideoId::parse("9Umm2MAKsMc").expect("valid id"),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(Strictness::Lenient),
        )
        .await
        .expect("player response");

    let calls = transport.calls();
    let body = body_of(&calls[1]).to_string();
    for forbidden in ["query", "params", "continuation", "cursor", "browseId"] {
        assert!(
            !body.contains(forbidden),
            "player body must not carry `{forbidden}`: {body}"
        );
    }
}

/// The player request is a web-client request: same header set as search, key only in the query
/// string, and no cookie unless a consent cookie was configured explicitly.
#[tokio::test]
async fn player_headers_match_the_web_client_profile() {
    let transport = replay(BOOTSTRAP, PLAYER_BODY);
    let provider = innertube(transport.clone());

    // Locale is taken from `ProviderContext`, which the engine builds from the API query; the
    // `hl`/`gl` fields on `ProviderRequest` are redundant for this provider (the context is
    // authoritative). Asserting the header alone would have hidden that, so both are checked below.
    let request_context = ProviderContext {
        hl: "de".into(),
        gl: "DE".into(),
        ..context(Strictness::Lenient)
    };

    provider
        .fetch(
            ProviderRequest::Video {
                id: VideoId::parse("9Umm2MAKsMc").expect("valid id"),
                hl: "de".into(),
                gl: "DE".into(),
            },
            &request_context,
        )
        .await
        .expect("player response");

    let calls = transport.calls();
    let player = &calls[1];
    assert_eq!(header(player, "content-type"), Some("application/json"));
    assert_eq!(header(player, "x-youtube-client-name"), Some("1"));
    assert_eq!(
        header(player, "x-youtube-client-version"),
        Some(provider.profile().version.as_str())
    );
    assert_eq!(header(player, "origin"), Some("https://www.youtube.com"));
    assert_eq!(header(player, "referer"), Some("https://www.youtube.com/"));
    assert_eq!(header(player, "accept-language"), Some("de,en;q=0.8"));
    assert_eq!(header(player, "x-goog-visitor-id"), Some("VISITOR-FIXTURE"));
    assert!(
        header(player, "cookie").is_none(),
        "no consent cookie was configured, so no cookie may be sent"
    );
    let user_agent = header(player, "user-agent").expect("user agent");
    assert!(user_agent.contains("Chrome/"), "{user_agent}");

    // The locale reaches the body too, not only the header.
    let body = body_of(player);
    assert_eq!(body["context"]["client"]["hl"], "de");
    assert_eq!(body["context"]["client"]["gl"], "DE");

    // The path is the player endpoint, not search, and the key stays in the query string.
    assert!(
        player.url.path().ends_with("/youtubei/v1/player"),
        "{}",
        player.url
    );
    assert_eq!(player.url.query_pairs().count(), 2);
    for (name, value) in &player.headers {
        assert!(
            !value.contains("AIza"),
            "the api key must never travel in header {name}"
        );
    }
}

// --------------------------------------------------------------------------------------------
// Bootstrap and key handling
// --------------------------------------------------------------------------------------------

/// The session document is fetched exactly once per refresh, and the api key never travels in a
/// header — it is only ever a query parameter (yt-dlp `_base.py:858-866`).
#[tokio::test]
async fn bootstrap_is_used_once_and_the_key_never_travels_in_a_header() {
    let transport = replay_many(BOOTSTRAP, &[SEARCH_BODY, SEARCH_BODY]);
    let provider = innertube(transport.clone());

    provider
        .fetch(search_request(None), &context(Strictness::Lenient))
        .await
        .expect("search response");
    provider
        .fetch(search_request(None), &context(Strictness::Lenient))
        .await
        .expect("second search response");

    let calls = transport.calls();
    assert_eq!(
        calls.len(),
        3,
        "one bootstrap for two searches: the session is cached for its TTL"
    );

    let bootstrap = &calls[0];
    assert_eq!(bootstrap.method, HttpMethod::Get);
    assert_eq!(bootstrap.url.as_str(), "https://www.youtube.com/sw.js_data");
    assert!(bootstrap.body.is_none());

    for call in &calls {
        for (name, value) in &call.headers {
            assert!(
                !value.contains("AIza"),
                "api key leaked into header {name}: {value}"
            );
        }
    }
}

/// Without a bootstrap key the request stays keyless rather than inventing one.
#[tokio::test]
async fn keyless_bootstrap_produces_a_keyless_request() {
    let transport = replay(r#"[[{"visitorData":"VISITOR-FIXTURE"}]]"#, SEARCH_BODY);
    let provider = innertube(transport.clone());

    provider
        .fetch(search_request(None), &context(Strictness::Lenient))
        .await
        .expect("search response");

    let calls = transport.calls();
    assert_eq!(
        calls[1].url.as_str(),
        "https://www.youtube.com/youtubei/v1/search?prettyPrint=false"
    );
    // The client version still comes from the profile when the bootstrap does not report one.
    assert_eq!(
        header(&calls[1], "x-youtube-client-version"),
        Some(provider.profile().version.as_str())
    );
}

/// Response mapping is wired to the request: a successful fetch returns the provider's own
/// provenance, and an empty document is a warning rather than an error in lenient mode.
#[tokio::test]
async fn empty_search_document_is_a_lenient_warning() {
    let transport = replay(BOOTSTRAP, SEARCH_BODY);
    let provider = innertube(transport);

    let response = provider
        .fetch(search_request(None), &context(Strictness::Lenient))
        .await
        .expect("lenient response");
    let ProviderResponse::Search(results) = response else {
        panic!("expected a search response");
    };
    assert!(results.items.is_empty());
    assert!(results.provenance.parse_warnings >= 1);

    // ... and an error in strict mode, where a missing renderer tree is not acceptable.
    let transport = replay(BOOTSTRAP, SEARCH_BODY);
    let provider = innertube(transport);
    let error = provider
        .fetch(search_request(None), &context(Strictness::Strict))
        .await
        .expect_err("strict mode must reject an unparseable page");
    assert_eq!(error.code(), "provider_parse");
}

// --------------------------------------------------------------------------------------------
// Comments
// --------------------------------------------------------------------------------------------

fn comments_fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/synthetic/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"))
}

fn comments_request(cursor: Option<&str>) -> ProviderRequest {
    ProviderRequest::Comments {
        id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
        sort: CommentSort::Top,
        cursor: cursor.map(str::to_owned),
        hl: "en".into(),
        gl: "US".into(),
        limit: 20,
    }
}

/// The comment section is reached in two documented steps and Vydra sends exactly those two bodies.
///
/// Step 1 is the watch page (`{context, videoId}`), which is where the section's continuation token is
/// read; step 2 is that token and nothing else (`{context, continuation}`). Neither body carries `params`,
/// and the second never repeats `videoId`.
///
/// References: `NewPipeExtractor` `YoutubeCommentsExtractor.getComments` (step 1 then step 2, same bodies)
/// and `LuanRT/YouTube.js` `Comments.getComments` (`{videoId}` for the watch page, then the token).
#[tokio::test]
async fn comments_ask_for_the_watch_page_then_post_only_the_relayed_token() {
    let watch_page = comments_fixture("comments_watch_page_with_section");
    let page1 = comments_fixture("comments_page1_top");
    let transport = replay_many(BOOTSTRAP, &[watch_page.as_str(), page1.as_str()]);
    let provider = innertube(transport.clone());

    let response = provider
        .fetch(comments_request(None), &context(Strictness::Lenient))
        .await
        .expect("comment page");
    let ProviderResponse::Comments(list) = response else {
        panic!("expected a comment list");
    };
    assert_eq!(list.items.len(), 3);

    let calls = transport.calls();
    assert_eq!(calls.len(), 3, "bootstrap, watch page, comment page");

    let watch = &calls[1];
    assert_eq!(
        watch.url.as_str(),
        "https://www.youtube.com/youtubei/v1/next?prettyPrint=false&key=AIzaSYFIXTUREKEY0123456789"
    );
    let body = body_of(watch);
    assert_eq!(sorted_keys(&body), vec!["context", "videoId"]);
    assert_eq!(body["videoId"], "dQw4w9WgXcQ");
    assert_eq!(body["context"]["client"]["clientName"], "WEB");
    assert_eq!(body["context"]["client"]["hl"], "en");
    assert_eq!(body["context"]["client"]["gl"], "US");

    let page = &calls[2];
    let body = body_of(page);
    assert_eq!(sorted_keys(&body), vec!["context", "continuation"]);
    assert_eq!(body["continuation"], "PLACEHOLDER-WATCH-SECTION-TOKEN");
    assert!(
        body.get("videoId").is_none(),
        "a continuation body never repeats the video id"
    );
    assert!(
        body.get("params").is_none(),
        "no params encoding is ever invented for comments"
    );
}

/// A cursor page needs no discovery pass: it is one `next` call with the relayed token.
#[tokio::test]
async fn a_comment_cursor_page_is_one_continuation_call() {
    let page1 = comments_fixture("comments_page1_top");
    let transport = replay(BOOTSTRAP, page1.as_str());
    let provider = innertube(transport.clone());

    provider
        .fetch(
            comments_request(Some("PLACEHOLDER-COMMENTS-PAGE2")),
            &context(Strictness::Lenient),
        )
        .await
        .expect("comment page");

    let calls = transport.calls();
    assert_eq!(calls.len(), 2, "bootstrap and one continuation call");
    let body = body_of(&calls[1]);
    assert_eq!(sorted_keys(&body), vec!["context", "continuation"]);
    assert_eq!(body["continuation"], "PLACEHOLDER-COMMENTS-PAGE2");
}

/// A token that is not a token is refused before any request is made.
#[tokio::test]
async fn a_continuation_that_is_not_a_token_is_refused_before_the_wire() {
    let page1 = comments_fixture("comments_page1_top");
    for bad in ["", "has space", "has\nnewline", "tab\there"] {
        let transport = replay(BOOTSTRAP, page1.as_str());
        let provider = innertube(transport.clone());
        let error = provider
            .fetch(
                comments_request(Some(bad)),
                &context(Strictness::Lenient),
            )
            .await
            .expect_err("must refuse");
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "expected invalid_request for {bad:?}, got {error:?}"
        );
        assert_eq!(
            transport.calls().len(),
            1,
            "only the bootstrap call may have happened"
        );
    }
}

/// Comments turned off is a content fact, not an empty page and not a parse error.
#[tokio::test]
async fn a_watch_page_without_a_comment_section_is_unavailable_comments_disabled() {
    let disabled = comments_fixture("comments_watch_page_without_section");
    let transport = replay(BOOTSTRAP, disabled.as_str());
    let provider = innertube(transport.clone());

    let error = provider
        .fetch(comments_request(None), &context(Strictness::Lenient))
        .await
        .expect_err("must not return an empty page");
    match error {
        ProviderError::Unavailable { reason, .. } => {
            assert_eq!(reason, UnavailabilityReason::CommentsDisabled);
            assert_eq!(reason.as_str(), "comments_disabled");
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
    assert_eq!(transport.calls().len(), 2, "no comment page was requested");
}

/// A reply page is reached only through the token the comment page rendered for that thread.
#[tokio::test]
async fn replies_scan_for_the_thread_and_then_post_its_token() {
    let watch_page = comments_fixture("comments_watch_page_with_section");
    let page1 = comments_fixture("comments_page1_top");
    let replies = comments_fixture("comments_replies_page1");
    let transport = replay_many(
        BOOTSTRAP,
        &[watch_page.as_str(), page1.as_str(), replies.as_str()],
    );
    let provider = innertube(transport.clone());

    let response = provider
        .fetch(
            ProviderRequest::CommentReplies {
                id: VideoId::parse("dQw4w9WgXcQ").expect("id"),
                parent: vydra_common::CommentId::parse("UgxAAAAAAAAAAAAAAAAAAAA").expect("id"),
                cursor: None,
                hl: "en".into(),
                gl: "US".into(),
                limit: 20,
            },
            &context(Strictness::Lenient),
        )
        .await
        .expect("reply page");
    let ProviderResponse::CommentReplies(page) = response else {
        panic!("expected a reply list");
    };
    assert_eq!(page.items.len(), 2);

    let calls = transport.calls();
    assert_eq!(calls.len(), 4, "bootstrap, watch page, comment page, replies");
    let last = body_of(&calls[3]);
    assert_eq!(sorted_keys(&last), vec!["context", "continuation"]);
    assert_eq!(last["continuation"], "PLACEHOLDER-REPLIES-THREAD1");
}
