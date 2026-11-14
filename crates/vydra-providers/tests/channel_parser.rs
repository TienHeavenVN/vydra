//! Channel browse mapping (Phase 6), driven by **synthetic** fixtures and a replay transport.
//!
//! Every fixture used here declares `"class": "synthetic"` and says so in its sidecar; none of them is a
//! capture. What this module proves is therefore *what the parser does with a documented shape*, not that
//! upstream answers in that shape today — `docs/research/CHANNEL_CONTRACT.md` §9 lists the difference and
//! `docs/reports/PHASE_6_REPORT.md` states which claims are fixture-verified and which are live-verified.
//!
//! The browse path is tested through the provider (not only through the mapper) because the first page of a
//! channel's videos needs **two** requests: the tab `params` blob is relayed from the channel page instead
//! of being hard-coded. A unit test on the mapper alone would not notice if the relay silently stopped
//! happening.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use time::OffsetDateTime;
use vydra_common::{ChannelId, ChannelRef};
use vydra_domain::{Availability, FieldSource, Precision, Provenance};
use vydra_errors::{ProviderError, UnavailabilityReason};
use vydra_providers::innertube::browse_map::{map_channel, map_channel_videos, videos_tab_params};
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::parse::ParseReport;
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport, Strictness,
    TransportResponse,
};

const CHANNEL: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
const BOOTSTRAP: &str = r#"[[{"visitorData":"VIS"}],"AIzaSYFIXTUREKEY0123456789"]"#;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

fn payload(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).expect("fixture is JSON")
}

fn provenance() -> Provenance {
    Provenance {
        provider: "innertube-web".into(),
        endpoint: "browse".into(),
        client: Some("web".into()),
        fetched_at: OffsetDateTime::now_utc(),
        cached: false,
        parse_warnings: 0,
        sources: vec![],
    }
}

fn context(strictness: Strictness) -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "channel-fixture".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness,
    }
}

fn ok(body: &str) -> Result<TransportResponse, vydra_providers::TransportError> {
    Ok(TransportResponse {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: Bytes::from(body.to_owned()),
        elapsed: Duration::from_millis(4),
    })
}

/// A provider whose transport replays the bootstrap and then the given documents, in order.
fn provider_with(bodies: &[String]) -> (InnertubeProvider, Arc<ReplayTransport>) {
    let mut responses = vec![ok(BOOTSTRAP)];
    responses.extend(bodies.iter().map(|body| ok(body)));
    let transport = Arc::new(ReplayTransport::new(responses));
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    (InnertubeProvider::new(config, transport.clone()), transport)
}

async fn channel(
    provider: &InnertubeProvider,
    reference: &str,
    strictness: Strictness,
) -> Result<vydra_domain::ChannelDocument, ProviderError> {
    match provider
        .fetch(
            ProviderRequest::Channel {
                reference: ChannelRef::parse(reference).expect("valid reference"),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(strictness),
        )
        .await?
    {
        ProviderResponse::Channel(document) => Ok(*document),
        other => panic!("expected a channel response, got {other:?}"),
    }
}

// --------------------------------------------------------------------------------------------
// Metadata, header and tabs
// --------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_channel_page_yields_metadata_header_counts_tabs_and_evidence() {
    let (provider, transport) = provider_with(&[fixture("synthetic/browse_channel_page.json")]);
    let document = channel(&provider, CHANNEL, Strictness::Strict)
        .await
        .expect("document");

    // Identity: the canonical id from `externalId`, never the requested string echoed back.
    assert_eq!(document.summary.id.as_str(), CHANNEL);
    assert_eq!(document.summary.title, "Placeholder Studio");
    assert_eq!(document.summary.handle.as_deref(), Some("placeholderstudio"));
    assert_eq!(
        document.summary.description.as_deref(),
        Some("Placeholder channel description.\nSecond line of the description.")
    );

    // Counts: parsed from rendered text, so they are approximate and keep the upstream wording.
    let subscribers = document
        .summary
        .subscriber_count
        .as_ref()
        .expect("subscriber count");
    assert_eq!(subscribers.value, 1_240_000);
    assert_eq!(subscribers.precision, Precision::Approximate);
    assert_eq!(subscribers.raw.as_deref(), Some("1.24M subscribers"));

    let videos = document.summary.video_count.as_ref().expect("video count");
    assert_eq!(videos.value, 512);
    assert_eq!(videos.precision, Precision::Exact);

    // Thumbnails: smallest → largest, and the banner is the largest candidate.
    let avatar_widths: Vec<Option<u32>> = document
        .summary
        .thumbnails
        .iter()
        .map(|thumb| thumb.width)
        .collect();
    assert_eq!(avatar_widths, vec![Some(48), Some(160)]);
    let banner = document.banner.as_ref().expect("banner");
    assert_eq!(banner.width, Some(2120));

    // Tabs: slugs only for titles we recognize, in document order, with the selected one marked.
    let slugs: Vec<Option<&str>> = document
        .tabs
        .iter()
        .map(|tab| tab.slug.as_deref())
        .collect();
    assert_eq!(
        slugs,
        vec![
            Some("home"),
            Some("videos"),
            Some("shorts"),
            Some("playlists"),
            Some("about")
        ]
    );
    assert!(document.tabs[1].selected);
    assert!(!document.tabs[0].selected);

    // Verification is reported as the badge upstream rendered, not as a boolean verdict.
    assert_eq!(document.verification.len(), 1);
    assert_eq!(document.verification[0].style, "BADGE_STYLE_TYPE_VERIFIED");
    assert_eq!(document.verification[0].label.as_deref(), Some("Verified"));

    // Fields this phase does not fetch stay absent rather than being invented.
    assert!(document.links.is_empty());
    assert!(document.joined_at.is_none());
    assert!(document.country.is_none());
    assert!(document.total_view_count.is_none());

    // Evidence names the sections that were read.
    assert!(document.evidence.has_section(FieldSource::ChannelMetadata));
    assert!(document.evidence.has_section(FieldSource::ChannelHeader));
    assert!(document.evidence.has_section(FieldSource::Tabs));
    assert!(document.evidence.fallbacks.is_empty());

    assert_eq!(document.availability, Availability::Available);
    assert_eq!(document.provenance.endpoint, "browse");
    assert_eq!(document.provenance.parse_warnings, 0);

    // One browse call (plus the session bootstrap): a channel document needs no tab request.
    assert_eq!(transport.call_count(), 2);
}

#[test]
fn a_single_count_part_is_not_guessed_to_be_subscribers_or_videos() {
    let body = payload("synthetic/browse_channel_page_ambiguous_counts.json");
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse("UCdddddddddddddddddddddd").expect("valid id");
    let document = map_channel(&body, &reference, &mut report, provenance()).expect("document");

    // The page stated exactly one count. Assigning it to either field would be a guess, so both stay
    // absent and the ambiguity is recorded.
    assert!(document.summary.subscriber_count.is_none());
    assert!(document.summary.video_count.is_none());
    assert!(report.count() >= 1);
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("channel_counts_ambiguous")),
        "expected the ambiguity to be reported, got {:?}",
        report.messages()
    );

    // A header without a banner is absence, not an empty banner, and not an error.
    assert!(document.banner.is_none());
    assert!(document.verification.is_empty());
}

#[test]
fn a_missing_channel_is_a_content_fact_and_not_a_retry() {
    let body = payload("synthetic/browse_channel_missing_alert.json");
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse("UCeeeeeeeeeeeeeeeeeeeeee").expect("valid id");
    let error = map_channel(&body, &reference, &mut report, provenance()).expect_err("error");

    match &error {
        ProviderError::NotFound { id, .. } => assert_eq!(id, "UCeeeeeeeeeeeeeeeeeeeeee"),
        other => panic!("expected not-found, got {other:?}"),
    }
    // A content fact must never be retried on another provider.
    assert!(!error.is_fallback_eligible());
}

#[test]
fn a_handle_is_checked_against_the_page_it_resolved_to() {
    let body = payload("synthetic/browse_channel_page.json");

    // The page belongs to @placeholderstudio, so a handle request for it resolves to the canonical id.
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse("@placeholderstudio").expect("valid handle");
    let document = map_channel(&body, &reference, &mut report, provenance()).expect("document");
    assert_eq!(document.summary.id.as_str(), CHANNEL);

    // A different handle must not be served the same page: that would be an alias for someone else's
    // channel, and `ChannelSummary.handle` would then disagree with the document's id.
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse("@someoneelse").expect("valid handle");
    let error = map_channel(&body, &reference, &mut report, provenance()).expect_err("mismatch");
    assert!(matches!(error, ProviderError::Parse { .. }));
}

#[test]
fn a_channel_id_redirect_is_served_with_the_canonical_id_and_recorded() {
    let body = payload("synthetic/browse_channel_page.json");
    let mut report = ParseReport::new();
    // Localized and auto-generated channels can answer under a different canonical id.
    let reference = ChannelRef::parse("UCffffffffffffffffffffff").expect("valid id");
    let document = map_channel(&body, &reference, &mut report, provenance()).expect("document");

    assert_eq!(document.summary.id.as_str(), CHANNEL);
    let conflict = document
        .evidence
        .conflicts
        .iter()
        .find(|conflict| conflict.field == "id")
        .expect("redirect is recorded as evidence");
    assert_eq!(conflict.kept_value, CHANNEL);
    assert_eq!(conflict.rejected_value, "UCffffffffffffffffffffff");
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("channel_id_redirected"))
    );
}

// --------------------------------------------------------------------------------------------
// Listing: two-step first page, token-only continuation, chip tokens are not cursors
// --------------------------------------------------------------------------------------------

#[test]
fn the_videos_tab_params_are_relayed_from_the_page_and_never_hard_coded() {
    let body = payload("synthetic/browse_channel_page.json");
    let params = videos_tab_params(&body).expect("videos tab params");
    assert_eq!(params, "EgZ2aWRlb3PyBgQKAjoA");

    // A page with no videos tab yields nothing to relay — which is an unsupported resource, not a
    // reason to fall back to a constant.
    let no_videos = json!({"contents": {"twoColumnBrowseResultsRenderer": {"tabs": [
        {"tabRenderer": {"title": "Shorts", "endpoint": {"browseEndpoint": {"params": "EgZzaG9ydHM"}}}}
    ]}}});
    assert!(videos_tab_params(&no_videos).is_none());
}

#[tokio::test]
async fn the_first_page_of_channel_videos_relays_the_tab_params_in_a_second_browse_call() {
    let (provider, transport) = provider_with(&[
        fixture("synthetic/browse_channel_page.json"),
        fixture("synthetic/browse_channel_videos_page1.json"),
    ]);

    let response = provider
        .fetch(
            ProviderRequest::ChannelVideos {
                id: ChannelId::parse(CHANNEL).expect("valid id"),
                cursor: None,
                hl: "en".into(),
                gl: "US".into(),
                limit: 50,
            },
            &context(Strictness::Strict),
        )
        .await
        .expect("list");

    let ProviderResponse::ChannelVideos(list) = response else {
        panic!("expected a channel listing");
    };

    // Three calls: bootstrap, channel page (for the params), videos tab.
    assert_eq!(transport.call_count(), 3);
    let calls = transport.calls();
    let tab_body = String::from_utf8_lossy(&calls[2].body.clone().expect("body")).to_string();
    assert!(
        tab_body.contains("EgZ2aWRlb3PyBgQKAjoA"),
        "the tab params must be relayed from the channel page: {tab_body}"
    );
    assert!(tab_body.contains(CHANNEL));

    // The duplicated row is counted, not served twice.
    assert_eq!(list.duplicates_removed, 1);
    let ids: Vec<&str> = list.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, vec!["vid00000000", "vid00000001", "vid00000002", "vid00000003"]);

    // The cursor is the page continuation, not the sort chip's token.
    assert_eq!(list.next_cursor.as_deref(), Some("PAGE1-CONTINUATION-TOKEN"));
    // Duplicate rows are accounted for in `duplicates_removed`, not in `parse_warnings`: a repeated row
    // is an upstream anomaly that must not turn a strict parse of an otherwise good page into an error.
    assert_eq!(list.provenance.parse_warnings, 0);
}

#[tokio::test]
async fn a_continuation_page_sends_only_the_token() {
    let (provider, transport) = provider_with(&[fixture(
        "synthetic/browse_channel_videos_page2.json",
    )]);

    let response = provider
        .fetch(
            ProviderRequest::ChannelVideos {
                id: ChannelId::parse(CHANNEL).expect("valid id"),
                cursor: Some("PAGE1-CONTINUATION-TOKEN".into()),
                hl: "en".into(),
                gl: "US".into(),
                limit: 50,
            },
            &context(Strictness::Strict),
        )
        .await
        .expect("list");

    let ProviderResponse::ChannelVideos(list) = response else {
        panic!("expected a channel listing");
    };
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.next_cursor.as_deref(), Some("PAGE2-CONTINUATION-TOKEN"));

    // Two calls: bootstrap and one continuation. No channel page is fetched for a continuation, and the
    // body carries the token and nothing that could mean "a different query".
    assert_eq!(transport.call_count(), 2);
    let body = String::from_utf8_lossy(&transport.calls()[1].body.clone().expect("body")).to_string();
    assert!(body.contains("PAGE1-CONTINUATION-TOKEN"));
    assert!(!body.contains("browseId"), "continuation body: {body}");
    assert!(!body.contains("params"), "continuation body: {body}");
}

#[test]
fn a_sort_menu_token_is_not_a_page_cursor() {
    let body = payload("synthetic/browse_channel_videos_sort_only.json");
    let mut report = ParseReport::new();
    let list = map_channel_videos(&body, 50, &mut report, provenance());

    assert_eq!(list.items.len(), 1);
    // Both sort tokens are `continuationCommand`s; neither may become `next_cursor`.
    assert!(list.next_cursor.is_none());
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("continuation_without_page_cursor")),
        "expected the dropped token to be reported, got {:?}",
        report.messages()
    );
}

#[test]
fn a_page_is_trimmed_to_the_requested_limit_and_says_so() {
    let body = payload("synthetic/browse_channel_videos_page1.json");
    let mut report = ParseReport::new();
    let list = map_channel_videos(&body, 2, &mut report, provenance());
    assert_eq!(list.items.len(), 2);
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("page_truncated"))
    );
}

#[test]
fn an_unknown_content_shape_never_becomes_an_item() {
    let body = json!({
        "contents": {"richGridRenderer": {"contents": [
            {"verticalListRenderer": {"items": [
                {"mysteryViewModel": {"whatever": true}},
                {"videoRenderer": {"videoId": "vid00000005", "title": {"simpleText": "kept"}}}
            ]}}
        ]}}
    });
    let mut report = ParseReport::new();
    let list = map_channel_videos(&body, 50, &mut report, provenance());
    assert_eq!(list.items.len(), 1);
    // `mysteryViewModel` is not a `*Renderer` key, so it is not counted as an unknown renderer; what
    // matters is that it never becomes an item.
    assert_eq!(list.items[0].title, "kept");
}

#[test]
fn an_error_page_without_metadata_is_not_served_as_an_empty_channel() {
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse(CHANNEL).expect("valid id");
    let error = map_channel(&json!({"contents": {}}), &reference, &mut report, provenance())
        .expect_err("error");
    assert!(matches!(error, ProviderError::Parse { .. }));
    assert!(report.has_structural_problem());
}

#[test]
fn an_age_gated_channel_reports_age_restriction_rather_than_an_empty_document() {
    let body = json!({
        "header": {"channelAgeGateRenderer": {"channelId": CHANNEL, "isAgeRestricted": true}},
        "contents": {}
    });
    let mut report = ParseReport::new();
    let reference = ChannelRef::parse(CHANNEL).expect("valid id");
    let error = map_channel(&body, &reference, &mut report, provenance()).expect_err("error");
    match error {
        ProviderError::Unavailable { reason, .. } => {
            assert_eq!(reason, UnavailabilityReason::AgeRestricted);
        }
        other => panic!("expected age restriction, got {other:?}"),
    }
}

#[tokio::test]
async fn a_truncated_browse_response_fails_closed() {
    // Malformed input must not become a partial channel: the JSON does not parse, so the provider
    // returns a typed parse error and no document at all.
    let (provider, _) = provider_with(&[fixture("malformed/browse_truncated.json")]);
    let error = channel(&provider, CHANNEL, Strictness::Lenient)
        .await
        .expect_err("truncated JSON must not parse");
    match error {
        ProviderError::Parse { message } => assert!(message.contains("invalid json")),
        other => panic!("expected a parse error, got {other:?}"),
    }
}
