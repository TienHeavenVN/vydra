//! Playlist browse mapping (Phase 6), driven by **synthetic** fixtures and a replay transport.
//!
//! The subject of these tests is the *entry list*, not the header. A playlist is an ordered collection
//! whose owner can delete, privatize or repeat entries at any time, so the two failure modes that matter
//! are:
//!
//! * **silently shifting positions** — dropping an unplayable row would renumber every later entry;
//! * **inventing availability** — treating a row without a playability flag as playable.
//!
//! Every fixture here is `"class": "synthetic"` and its sidecar says so; nothing in this module claims a
//! live shape. See `docs/research/PLAYLIST_CONTRACT.md` §9 for the verification boundary.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{Value, json};
use time::OffsetDateTime;
use vydra_common::PlaylistId;
use vydra_domain::{Availability, FieldSource, PlaylistEntrySource, Provenance};
use vydra_errors::{ProviderError, UnavailabilityReason};
use vydra_providers::innertube::browse_map::{map_playlist, map_playlist_videos};
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::parse::ParseReport;
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport, Strictness,
    TransportResponse,
};

const PLAYLIST: &str = "PLcccccccccccccccccccccc";
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

fn playlist_id() -> PlaylistId {
    PlaylistId::parse(PLAYLIST).expect("valid playlist id")
}

fn context(strictness: Strictness) -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "playlist-fixture".into(),
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

// --------------------------------------------------------------------------------------------
// Document
// --------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_playlist_page_yields_header_sidebar_and_ordered_entries() {
    let (provider, transport) = provider_with(&[fixture("synthetic/browse_playlist_page.json")]);
    let response = provider
        .fetch(
            ProviderRequest::Playlist {
                id: playlist_id(),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(Strictness::Strict),
        )
        .await
        .expect("document");
    let ProviderResponse::Playlist(document) = response else {
        panic!("expected a playlist document");
    };

    assert_eq!(document.summary.id.as_str(), PLAYLIST);
    assert_eq!(document.summary.title, "Placeholder playlist");
    assert_eq!(
        document.summary.description.as_deref(),
        Some("A playlist used to exercise the parser.")
    );
    // Owner identity comes from the header's owner runs, and the id is validated as a channel id.
    assert_eq!(
        document.summary.channel_id.as_ref().map(|id| id.as_str()),
        Some("UCbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(
        document.summary.channel_title.as_deref(),
        Some("Placeholder Owner")
    );
    // "1,204 videos" is a rendered label, but it carries no abbreviation: it is a plain number.
    let count = document.summary.item_count.as_ref().expect("item count");
    assert_eq!(count.value, 1_204);
    assert_eq!(document.view_count.as_ref().map(|c| c.value), Some(3_456));
    assert_eq!(
        document.last_updated_text.as_deref(),
        Some("Updated 3 days ago")
    );

    // Entries: four rows, positions 1..4, nothing dropped and nothing renumbered.
    let positions: Vec<Option<u32>> = document.items.iter().map(|item| item.position).collect();
    assert_eq!(
        positions,
        vec![Some(1), Some(2), Some(3), Some(4)],
        "positions must come from the row's own index"
    );
    assert_eq!(document.items.len(), 4);

    // The unplayable row keeps its position and its verbatim title, and carries a validated id.
    let deleted = &document.items[1];
    assert_eq!(deleted.title, "[Deleted video]");
    assert_eq!(
        deleted.availability,
        Availability::Unavailable {
            reason: UnavailabilityReason::Other
        }
    );
    assert_eq!(
        deleted.video_id.as_ref().map(|id| id.as_str()),
        Some("AAAAAAAAAAA")
    );
    assert!(!deleted.is_playable());

    // A repeated video is legitimate in a playlist: it is counted, not dropped.
    assert_eq!(document.repeated_entries, Some(1));
    assert_eq!(
        document.items[3].video_id.as_ref().map(|id| id.as_str()),
        Some("vid00000000")
    );

    // Durations: the exact `lengthSeconds` field wins over the rendered clock text.
    assert_eq!(document.items[0].duration_seconds, Some(601));

    assert_eq!(
        document.next_cursor.as_deref(),
        Some("PLAYLIST-PAGE-1-TOKEN")
    );

    // Evidence names every section that actually contributed.
    assert!(document.evidence.has_section(FieldSource::PlaylistMetadata));
    assert!(document.evidence.has_section(FieldSource::PlaylistHeader));
    assert!(document.evidence.has_section(FieldSource::PlaylistSidebar));
    assert!(document.evidence.has_section(FieldSource::PlaylistVideoList));

    assert_eq!(document.provenance.parse_warnings, 0);
    assert_eq!(transport.call_count(), 2, "bootstrap + one browse call");
}

#[test]
fn a_deleted_entry_without_an_id_keeps_its_position() {
    // A row whose `videoId` is not a valid id: the entry must still appear, so positions do not shift,
    // but it must not carry a video reference either.
    let body = json!({
        "metadata": {"playlistMetadataRenderer": {"title": "Mixed"}},
        "contents": {"twoColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {
            "playlistVideoListRenderer": {"contents": [
                {"playlistVideoRenderer": {"index": {"simpleText": "1"}, "videoId": "vid00000000",
                    "title": {"simpleText": "first"}, "isPlayable": true}},
                {"playlistVideoRenderer": {"index": {"simpleText": "2"}, "videoId": "not-an-id",
                    "title": {"simpleText": "[Deleted video]"}, "isPlayable": false}},
                {"playlistVideoRenderer": {"index": {"simpleText": "3"}, "videoId": "vid00000001",
                    "title": {"simpleText": "third"}, "isPlayable": true}}
            ]}
        }}}]}}
    });
    let mut report = ParseReport::new();
    let document = map_playlist(&body, &playlist_id(), 50, &mut report, provenance())
        .expect("document");

    assert_eq!(document.items.len(), 3);
    assert_eq!(document.items[1].position, Some(2));
    assert!(document.items[1].video_id.is_none());
    assert_eq!(document.items[2].position, Some(3));
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("playlist_entry_id_invalid")),
        "an invalid id must be reported, got {:?}",
        report.messages()
    );
}

#[test]
fn a_row_without_playability_is_unknown_and_not_available() {
    let body = json!({
        "metadata": {"playlistMetadataRenderer": {"title": "No verdict"}},
        "contents": {"twoColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {
            "playlistVideoListRenderer": {"contents": [
                {"playlistVideoRenderer": {"index": {"simpleText": "1"}, "videoId": "vid00000000",
                    "title": {"simpleText": "no isPlayable field"}}}
            ]}
        }}}]}}
    });
    let mut report = ParseReport::new();
    let document = map_playlist(&body, &playlist_id(), 50, &mut report, provenance())
        .expect("document");

    assert_eq!(document.items.len(), 1);
    assert_eq!(document.items[0].availability, Availability::Unknown);
    assert!(!document.items[0].is_playable());
    // An unknown verdict is never cached (ADR-022), which the engine enforces on `availability`; here it
    // is enough that the document does not claim playability.
}

#[test]
fn a_private_playlist_is_refused_instead_of_returned_empty() {
    let body = payload("synthetic/browse_playlist_private_alert.json");
    let mut report = ParseReport::new();
    let error =
        map_playlist(&body, &playlist_id(), 50, &mut report, provenance()).expect_err("error");
    match error {
        ProviderError::Unavailable { reason, message } => {
            assert_eq!(reason, UnavailabilityReason::Private);
            assert!(message.to_lowercase().contains("private"));
        }
        other => panic!("expected private unavailability, got {other:?}"),
    }
}

#[test]
fn a_missing_playlist_is_not_found_and_never_retried() {
    let body = payload("synthetic/browse_playlist_missing_alert.json");
    let mut report = ParseReport::new();
    let error =
        map_playlist(&body, &playlist_id(), 50, &mut report, provenance()).expect_err("error");
    match &error {
        ProviderError::NotFound { id, .. } => assert_eq!(id, PLAYLIST),
        other => panic!("expected not-found, got {other:?}"),
    }
    assert!(!error.is_fallback_eligible());
}

// --------------------------------------------------------------------------------------------
// Continuation pages and the `VL` browse id
// --------------------------------------------------------------------------------------------

#[test]
fn a_continuation_page_keeps_the_upstream_positions() {
    let body = payload("synthetic/browse_playlist_page2.json");
    let mut report = ParseReport::new();
    let page = map_playlist_videos(&body, 50, &mut report, provenance());

    assert_eq!(page.items.len(), 1);
    // Position 5, not 1: the offset inside a continuation page says nothing about the playlist order.
    assert_eq!(page.items[0].position, Some(5));
    assert_eq!(page.next_cursor.as_deref(), Some("PLAYLIST-PAGE-2-TOKEN"));
    assert_eq!(page.provenance.parse_warnings, 0);
}

#[tokio::test]
async fn the_playlist_browse_id_is_the_vl_prefixed_id() {
    let (provider, transport) = provider_with(&[fixture("synthetic/browse_playlist_page.json")]);
    let _ = provider
        .fetch(
            ProviderRequest::PlaylistVideos {
                id: playlist_id(),
                cursor: None,
                hl: "en".into(),
                gl: "US".into(),
                limit: 50,
            },
            &context(Strictness::Strict),
        )
        .await
        .expect("list");

    let body = String::from_utf8_lossy(&transport.calls()[1].body.clone().expect("body")).to_string();
    assert!(
        body.contains(&format!("\"browseId\":\"VL{PLAYLIST}\"")),
        "playlist pages are addressed as VL+id: {body}"
    );
    // No invented `params` on a playlist page.
    assert!(!body.contains("\"params\""));
}

#[tokio::test]
async fn a_playlist_continuation_sends_only_the_token() {
    let (provider, transport) = provider_with(&[fixture("synthetic/browse_playlist_page2.json")]);
    let response = provider
        .fetch(
            ProviderRequest::PlaylistVideos {
                id: playlist_id(),
                cursor: Some("PLAYLIST-PAGE-1-TOKEN".into()),
                hl: "en".into(),
                gl: "US".into(),
                limit: 50,
            },
            &context(Strictness::Strict),
        )
        .await
        .expect("list");
    let ProviderResponse::PlaylistVideos(page) = response else {
        panic!("expected a playlist listing");
    };
    assert_eq!(page.items.len(), 1);

    let body = String::from_utf8_lossy(&transport.calls()[1].body.clone().expect("body")).to_string();
    assert!(body.contains("PLAYLIST-PAGE-1-TOKEN"));
    assert!(!body.contains("browseId"));
    assert!(!body.contains("VL"), "continuation body must not re-state the list: {body}");
}

#[test]
fn an_entry_from_a_lockup_view_model_is_counted_as_unmapped() {
    // The newer `lockupViewModel` row carries neither `index` nor `isPlayable`; mapping it would mean
    // inventing a position and a verdict, so it is counted instead.
    let body = json!({
        "metadata": {"playlistMetadataRenderer": {"title": "Lockups"}},
        "contents": {"playlistVideoListRenderer": {"contents": [
            {"lockupViewModel": {"contentId": "vid00000000"}},
            {"playlistVideoRenderer": {"index": {"simpleText": "2"}, "videoId": "vid00000001",
                "title": {"simpleText": "kept"}, "isPlayable": true}}
        ]}}
    });
    let mut report = ParseReport::new();
    let page = map_playlist_videos(&body, 50, &mut report, provenance());
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source, PlaylistEntrySource::PlaylistVideo);
    assert!(report.unmapped_results() >= 1);
}
