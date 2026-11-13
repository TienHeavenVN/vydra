//! Fixture-driven provider tests.
//!
//! These tests are the contract: they run the **real** parsing and mapping code against recorded
//! payloads replayed through `ReplayTransport`, so a regression is caught without network access.
//! Fixture provenance is declared next to each file (see `fixtures/*.meta.json`).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::{
    Capability, Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport,
    SearchFilters, SearchKind, Strictness, TransportResponse,
};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

fn context(strictness: Strictness) -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "fixture-test".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness,
    }
}

fn provider_with_fixture(body: String) -> InnertubeProvider {
    let transport = Arc::new(ReplayTransport::new(vec![
        // session bootstrap (synthetic)
        Ok(TransportResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from_static(br#"[[{"visitorData":"VIS"}],"AIzaSYFIXTUREKEY0123456789"]#),
            elapsed: Duration::from_millis(1),
        }),
        // endpoint payload
        Ok(TransportResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from(body),
            elapsed: Duration::from_millis(7),
        }),
    ]));
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        enable_unverified_filters: true,
        ..Default::default()
    });
    InnertubeProvider::new(config, transport)
}

#[tokio::test]
async fn mixed_search_fixture_maps_to_domain_items() {
    let provider = provider_with_fixture(fixture("search_mixed.json"));
    let request = ProviderRequest::Search {
        query: "rick astley".into(),
        kind: SearchKind::All,
        filters: SearchFilters::default(),
        cursor: None,
        hl: "en".into(),
        gl: "US".into(),
        limit: 20,
    };

    let response = provider
        .fetch(request, &context(Strictness::Lenient))
        .await
        .expect("provider response");

    let ProviderResponse::Search(results) = response else {
        panic!("expected a search response");
    };

    assert_eq!(results.items.len(), 3);
    assert_eq!(results.items[0].kind(), "video");
    assert_eq!(results.items[1].kind(), "channel");
    assert_eq!(results.items[2].kind(), "playlist");
    assert_eq!(results.next_cursor.as_deref(), Some("CBQSCBg"));
    assert_eq!(results.estimated_results.map(|c| c.value), Some(4210));
    assert_eq!(results.provenance.provider, "innertube-web");
    assert_eq!(results.provenance.endpoint, "search");
    assert!(!results.provenance.cached);
}

#[tokio::test]
async fn capabilities_reflect_verified_functionality_only() {
    let transport = Arc::new(ReplayTransport::new(vec![]));

    let conservative = InnertubeProvider::new(
        Arc::new(InnertubeProviderConfig::default()),
        transport.clone(),
    );
    assert!(conservative.capabilities().contains(Capability::SearchAll));
    assert!(conservative.capabilities().contains(Capability::VideoMetadata));
    // Type-scoped search needs the protobuf params builder, which is unverified.
    assert!(!conservative.capabilities().contains(Capability::SearchVideos));
    assert!(!conservative.capabilities().contains(Capability::CommentsThread));

    let opted_in = InnertubeProvider::new(
        Arc::new(InnertubeProviderConfig {
            enable_unverified_filters: true,
            ..Default::default()
        }),
        transport,
    );
    assert!(opted_in.capabilities().contains(Capability::SearchVideos));
    assert!(opted_in.capabilities().contains(Capability::SearchFilters));
}

#[tokio::test]
async fn filters_are_rejected_when_the_capability_is_not_declared() {
    let provider = InnertubeProvider::new(
        Arc::new(InnertubeProviderConfig::default()),
        Arc::new(ReplayTransport::new(vec![])),
    );
    let request = ProviderRequest::Search {
        query: "x".into(),
        kind: SearchKind::All,
        filters: SearchFilters {
            sort: vydra_providers::SearchSort::ViewCount,
            ..Default::default()
        },
        cursor: None,
        hl: "en".into(),
        gl: "US".into(),
        limit: 20,
    };
    let error = provider
        .fetch(request, &context(Strictness::Lenient))
        .await
        .expect_err("filters must be refused");
    assert_eq!(error.code(), "unsupported");
}
