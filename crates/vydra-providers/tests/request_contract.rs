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
    Arc::new(ReplayTransport::new(vec![
        Ok(TransportResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from(bootstrap.to_owned()),
            elapsed: Duration::from_millis(1),
        }),
        Ok(TransportResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Bytes::from(payload.to_owned()),
            elapsed: Duration::from_millis(3),
        }),
    ]))
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

// --------------------------------------------------------------------------------------------
// Bootstrap and key handling
// --------------------------------------------------------------------------------------------

/// The session document is fetched exactly once per refresh, and the api key never travels in a
/// header — it is only ever a query parameter (yt-dlp `_base.py:858-866`).
#[tokio::test]
async fn bootstrap_is_used_once_and_the_key_never_travels_in_a_header() {
    let transport = replay(BOOTSTRAP, SEARCH_BODY);
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
