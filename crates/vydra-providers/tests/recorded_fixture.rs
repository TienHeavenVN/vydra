//! Tests driven by **recorded** upstream payloads.
//!
//! Unlike `search_fixture.rs` (synthetic documents written by hand), everything here is a reduced
//! subset of a real capture published by a third-party client. Provenance is declared next to each
//! file in `tests/fixtures/**/*.meta.json` and summarized in `docs/research/FIXTURE_PROVENANCE.md`.
//!
//! What these tests do **not** prove: that upstream still answers with these shapes, that the
//! continuation token is still accepted, or that any playback URL resolves. Vydra has never made a
//! live request; replaying a recording only proves the parser agrees with the recording.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use vydra_common::VideoId;
use vydra_domain::{Availability, CaptionKind, DeliveryKind, Precision};
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport, SearchFilters,
    SearchKind, Strictness, TransportResponse,
};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

fn context() -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "recorded-fixture".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness: Strictness::Lenient,
    }
}

/// Replay the bootstrap document plus one recorded payload.
fn provider_with(body: String) -> InnertubeProvider {
    let transport = Arc::new(ReplayTransport::new(vec![
        Ok(TransportResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from_static(br#"[[{"visitorData":"VIS"}],"AIzaSYFIXTUREKEY0123456789"]"#),
            elapsed: Duration::from_millis(1),
        }),
        Ok(TransportResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Bytes::from(body),
            elapsed: Duration::from_millis(7),
        }),
    ]));
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    InnertubeProvider::new(config, transport)
}

async fn search(body: String, cursor: Option<&str>) -> ProviderResponse {
    provider_with(body)
        .fetch(
            ProviderRequest::Search {
                query: "captain jack".into(),
                kind: SearchKind::All,
                filters: SearchFilters::default(),
                cursor: cursor.map(str::to_owned),
                hl: "en".into(),
                gl: "US".into(),
                limit: 20,
            },
            &context(),
        )
        .await
        .expect("search response")
}

// --------------------------------------------------------------------------------------------
// Recorded search page (MediaServiceCore, chrome channel search tab, 2023-12-06)
// --------------------------------------------------------------------------------------------

/// Every renderer in the recording maps, in document order, with the localized values preserved
/// verbatim. The page contains no continuation token, so `next_cursor` must stay `None` rather than
/// being invented.
#[tokio::test]
async fn recorded_search_page_maps_every_item() {
    let response = search(fixture("recorded/search_web_2023-12-06.json"), None).await;
    let ProviderResponse::Search(results) = response else {
        panic!("expected a search response");
    };

    let kinds: Vec<&str> = results.items.iter().map(|item| item.kind()).collect();
    assert_eq!(
        kinds,
        vec!["video", "video", "video", "video", "playlist", "video"],
        "document order must be preserved"
    );
    assert!(
        results.next_cursor.is_none(),
        "the recording has no continuationItemRenderer"
    );

    let Some(vydra_domain::SearchItem::Video(first)) = results.items.first() else {
        panic!("first item must be a video");
    };
    assert_eq!(first.id.as_str(), "uAnqcJr389M");
    assert!(
        first.title.contains("In The Army Now"),
        "title: {}",
        first.title
    );
    assert_eq!(first.duration_seconds, Some(185));
    // `"25 399 352 просмотра"` — NBSP-separated thousands and a localized noun.
    let views = first.view_count.expect("view count");
    assert_eq!(views.value, 25_399_352);
    assert_eq!(views.precision, Precision::Exact);
    assert_eq!(first.published_text.as_deref(), Some("6 лет назад"));
    assert_eq!(first.channel_title.as_deref(), Some("Captain Jack Official"));
    assert_eq!(
        first.channel_id.as_ref().map(|id| id.as_str()),
        Some("UCa8RiU2st_48AA4WscbwYVQ")
    );

    let Some(vydra_domain::SearchItem::Playlist(playlist)) = results.items.get(4) else {
        panic!("fifth item must be a playlist");
    };
    assert_eq!(
        playlist.id.as_str(),
        "PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR"
    );
}

// --------------------------------------------------------------------------------------------
// Recorded continuation page (MediaServiceCore, browse "what to watch", 2023-02-23)
// --------------------------------------------------------------------------------------------

/// The trailing token survives normalization. This is a **browse** continuation: it proves the
/// token-extraction path and the `richItemRenderer` shape, not that a *search* continuation exists.
#[tokio::test]
async fn recorded_continuation_page_keeps_the_trailing_token() {
    let response = search(
        fixture("recorded/continuation_browse_2023-02-23.json"),
        Some("CONTINUATION-FIXTURE"),
    )
    .await;
    let ProviderResponse::Search(results) = response else {
        panic!("expected a search response");
    };

    assert_eq!(results.items.len(), 5, "five items plus the token item");

    let ids: Vec<&str> = results
        .items
        .iter()
        .filter_map(|item| match item {
            vydra_domain::SearchItem::Video(video) => Some(video.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            "FQBMyNvBTLY",
            "XPGszel4E5w",
            "ZnQAshDwZaI",
            "Dy7w4g_gwIk",
            "hqRVz_4wCrc"
        ]
    );

    let token = results.next_cursor.expect("continuation token");
    assert_eq!(token.len(), 582, "recorded token length");
    assert_eq!(token.chars().take(8).collect::<String>(), "4qmFsgKr");

    // `hqRVz_4wCrc` is 2:32:20 — an hour-long clock that the duration parser must accept.
    let long = results.items.iter().find_map(|item| match item {
        vydra_domain::SearchItem::Video(video) if video.id.as_str() == "hqRVz_4wCrc" => Some(video),
        _ => None,
    });
    assert_eq!(long.and_then(|video| video.duration_seconds), Some(9152));
}

// --------------------------------------------------------------------------------------------
// Recorded player page (MediaServiceCore, web client, 2025-11-10)
// --------------------------------------------------------------------------------------------

/// Availability, format metadata and caption kind survive the recording.
///
/// The reduction deliberately removed `signatureCipher` and every URL query string, so the adaptive
/// entries no longer carry delivery information: they must be reported as `Unknown`, never as
/// `Direct`. The single muxed format kept its URL and is therefore `Direct`.
#[tokio::test]
async fn recorded_player_page_maps_availability_formats_and_captions() {
    let provider = provider_with(fixture("recorded/player_web_2025-11-10.json"));
    let id = VideoId::parse("9Umm2MAKsMc").expect("fixture id");

    let response = provider
        .fetch(
            ProviderRequest::Video {
                id: id.clone(),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(),
        )
        .await
        .expect("player response");
    let ProviderResponse::Video(document) = response else {
        panic!("expected a video response");
    };

    assert_eq!(document.availability, Availability::Available);
    assert_eq!(document.summary.id, id);
    assert_eq!(document.summary.duration_seconds, Some(3537));
    assert_eq!(
        document.summary.view_count.as_ref().map(|count| count.value),
        Some(31_834)
    );
    assert_eq!(
        document.summary.channel_id.as_ref().map(|id| id.as_str()),
        Some("UCTk3OjLsX-sYl60JBq3z5QQ")
    );
    assert_eq!(document.category.as_deref(), Some("Gaming"));
    assert!(
        document.summary.published_at.is_some(),
        "microformat publishDate must be parsed"
    );

    assert_eq!(document.formats.len(), 4, "3 adaptive + 1 muxed");
    assert_eq!(
        document.formats[3].delivery,
        DeliveryKind::Direct,
        "the muxed format kept its URL"
    );
    for format in &document.formats {
        assert!(
            !matches!(
                format.delivery,
                DeliveryKind::Drm | DeliveryKind::SignatureProtected | DeliveryKind::TokenProtected
            ),
            "the reduction removed signature data, so nothing may claim protection: {format:?}"
        );
    }
    assert_eq!(document.formats[0].itag, Some(308));
    assert_eq!(document.formats[0].height, Some(1440));

    assert_eq!(document.caption_tracks.len(), 1);
    let track = &document.caption_tracks[0];
    assert_eq!(track.language_code, "ru");
    assert_eq!(track.kind, CaptionKind::Automatic);
    assert!(
        track.base_url_present,
        "the recording keeps a baseUrl presence signal"
    );
}

// --------------------------------------------------------------------------------------------
// Malformed / incomplete documents
// --------------------------------------------------------------------------------------------

/// A search page with no renderer tree is an empty page plus a warning, not a crash and not a
/// fabricated list.
#[tokio::test]
async fn malformed_search_without_contents_is_an_empty_page() {
    let response = search(fixture("malformed/search_without_contents.json"), None).await;
    let ProviderResponse::Search(results) = response else {
        panic!("expected a search response");
    };
    assert!(results.items.is_empty());
    assert!(results.next_cursor.is_none());
    assert!(
        results.provenance.parse_warnings >= 1,
        "the missing renderer tree must be recorded"
    );

    // Strict mode turns the same document into a parse error.
    let provider = provider_with(fixture("malformed/search_without_contents.json"));
    let error = provider
        .fetch(
            ProviderRequest::Search {
                query: "x".into(),
                kind: SearchKind::All,
                filters: SearchFilters::default(),
                cursor: None,
                hl: "en".into(),
                gl: "US".into(),
                limit: 20,
            },
            &ProviderContext {
                strictness: Strictness::Strict,
                ..context()
            },
        )
        .await
        .expect_err("strict mode must reject a page with no renderers");
    assert_eq!(error.code(), "provider_parse");
}

/// A player response without `playabilityStatus` keeps the data it has and records the gap instead
/// of guessing a reason.
#[tokio::test]
async fn malformed_player_without_playability_warns_but_keeps_the_document() {
    let provider = provider_with(fixture("malformed/player_without_playability.json"));
    let id = VideoId::parse("9Umm2MAKsMc").expect("fixture id");

    let response = provider
        .fetch(
            ProviderRequest::Video {
                id,
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(),
        )
        .await
        .expect("lenient response");
    let ProviderResponse::Video(document) = response else {
        panic!("expected a video response");
    };
    assert_eq!(document.availability, Availability::Available);
    assert!(
        document.provenance.parse_warnings >= 1,
        "missing playabilityStatus must be recorded"
    );
}

/// Truncated JSON is a decode failure, surfaced as `provider_parse` — never a partially parsed page.
#[tokio::test]
async fn truncated_search_document_is_a_parse_error() {
    let provider = provider_with(fixture("malformed/search_truncated.json"));
    let error = provider
        .fetch(
            ProviderRequest::Search {
                query: "x".into(),
                kind: SearchKind::All,
                filters: SearchFilters::default(),
                cursor: None,
                hl: "en".into(),
                gl: "US".into(),
                limit: 20,
            },
            &context(),
        )
        .await
        .expect_err("truncated document must not parse");
    assert_eq!(error.code(), "provider_parse");
}
