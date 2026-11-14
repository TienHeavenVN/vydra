//! Player (`POST /youtubei/v1/player`) behaviour, driven by **live captures made by Vydra itself**.
//!
//! Provenance for every file used here is in `docs/research/PLAYER_FIXTURE_PROVENANCE.md`; the raw
//! bytes are archived in `docs/research/data/player-probe-43549736295/` and can be restored with
//! `gunzip -c`. This module proves what the parser *does with those bytes* — not that upstream still
//! answers the same way today.
//!
//! The three live shapes (capture run 43549736295, 2026-11-14):
//!
//! | Capture | Verdict | `videoDetails` | Vydra today |
//! | --- | --- | --- | --- |
//! | public video | [`UNPLAYABLE`] "The page needs to be reloaded." | present | document, `availability.unavailable{other}` |
//! | public video | [`LOGIN_REQUIRED`] "Sign in to confirm you're not a bot" | absent | `409 content_unavailable{login_required}` |
//! | non-existent id | [`ERROR`] "This video is unavailable" | absent | `409 content_unavailable{other}` |
//!
//! A `UNPLAYABLE` verdict is not an error and a missing `videoDetails` is not a parse failure: the
//! two facts are independent, and conflating them was the Phase 5A defect this module pins.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use time::UtcOffset;
use vydra_common::VideoId;
use vydra_domain::{Availability, FieldSource, LiveState};
use vydra_errors::{ProviderError, UnavailabilityReason};
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeProvider, InnertubeProviderConfig,
};
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReplayTransport, Strictness,
    TransportResponse,
};

const PUBLIC_ID: &str = "dQw4w9WgXcQ";

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

fn context(strictness: Strictness) -> ProviderContext {
    ProviderContext {
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        request_id: "player-fixture".into(),
        hl: "en".into(),
        gl: "US".into(),
        strictness,
    }
}

/// One bootstrap document followed by one player response with the given status.
fn provider_with_status(body: &str, status: u16) -> (InnertubeProvider, Arc<ReplayTransport>) {
    let transport = Arc::new(ReplayTransport::new(vec![
        Ok(TransportResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from_static(br#"[[{"visitorData":"VIS"}],"AIzaSYFIXTUREKEY0123456789"]"#),
            elapsed: Duration::from_millis(1),
        }),
        Ok(TransportResponse {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: Bytes::from(body.to_owned()),
            elapsed: Duration::from_millis(7),
        }),
    ]));
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    (InnertubeProvider::new(config, transport.clone()), transport)
}

fn provider_with(body: &str) -> InnertubeProvider {
    provider_with_status(body, 200).0
}

async fn video(
    provider: &InnertubeProvider,
    id: &str,
    strictness: Strictness,
) -> Result<ProviderResponse, ProviderError> {
    provider
        .fetch(
            ProviderRequest::Video {
                id: VideoId::parse(id).expect("valid id"),
                hl: "en".into(),
                gl: "US".into(),
            },
            &context(strictness),
        )
        .await
}

fn document(response: ProviderResponse) -> vydra_domain::VideoDocument {
    match response {
        ProviderResponse::Video(document) => *document,
        other => panic!("expected a video response, got {other:?}"),
    }
}

fn unavailability(error: ProviderError) -> (UnavailabilityReason, String) {
    match error {
        ProviderError::Unavailable { reason, message } => (reason, message),
        other => panic!("expected a typed unavailability, got {other:?}"),
    }
}

// --------------------------------------------------------------------------------------------
// Live capture: UNPLAYABLE but a complete metadata document
// --------------------------------------------------------------------------------------------

/// The public-video capture: playback is refused ("the page needs to be reloaded") while the
/// metadata is present and correct. Vydra keeps the metadata and reports the verdict explicitly.
#[tokio::test]
async fn live_unplayable_capture_keeps_metadata_and_reports_the_verdict() {
    let provider = provider_with(&fixture(
        "recorded/player_probe_public_unplayable_2026-11-14.json",
    ));
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Lenient)
            .await
            .expect("document"),
    );

    // Availability is a value, not a lie: nothing about this document says "playable".
    assert_eq!(
        document.availability,
        Availability::Unavailable {
            reason: UnavailabilityReason::Other
        }
    );
    assert!(!document.availability.is_available());

    assert_eq!(
        document.summary.title,
        "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)"
    );
    assert_eq!(document.summary.duration_seconds, Some(213));
    assert_eq!(
        document
            .summary
            .view_count
            .as_ref()
            .map(|count| count.value),
        Some(1_836_979_877)
    );
    assert_eq!(
        document.summary.channel_id.as_ref().map(|id| id.as_str()),
        Some("UCuAXFkgsw1L7xaCfnd5JJOw")
    );
    assert_eq!(
        document.summary.channel_title.as_deref(),
        Some("Rick Astley")
    );
    assert_eq!(document.category.as_deref(), Some("Music"));
    assert_eq!(document.keywords.len(), 27);
    assert_eq!(
        document.keywords.first().map(String::as_str),
        Some("rick astley")
    );

    // The description is what chapter extraction reads, so its presence matters.
    let description = document.description.as_deref().expect("description");
    assert!(
        description.starts_with("The official video for"),
        "{description}"
    );
    assert!(description.len() > 1_000);

    // `publishDate` is `2009-10-24T23:57:33-07:00`. ADR-016 says timestamps are RFC 3339 **UTC**, so
    // the same instant must arrive normalised as 2009-10-25T06:57:33Z; keeping the publisher's offset
    // would give one instant two wire forms.
    let published = document.summary.published_at.expect("publish date");
    assert_eq!(published.offset(), UtcOffset::UTC);
    assert_eq!(published.year(), 2009);
    assert_eq!(published.day(), 25);
    assert_eq!(published.hour(), 6);
    assert_eq!(published.minute(), 57);
    assert_eq!(published.second(), 33);

    // Thumbnails come from the microformat and are normalised to absolute https URLs.
    assert_eq!(document.summary.thumbnails.len(), 1);
    assert_eq!(document.summary.thumbnails[0].width, Some(1280));
    assert!(
        document.summary.thumbnails[0]
            .url
            .starts_with("https://i.ytimg.com/"),
        "{}",
        document.summary.thumbnails[0].url
    );

    // No `streamingData` and no `captions` in this answer: empty, not invented, and no `no_formats`
    // warning for an unavailable document (there is nothing to play, by upstream's own verdict).
    assert!(document.formats.is_empty());
    assert!(document.caption_tracks.is_empty());

    // A non-OK verdict is a *content* fact, not a parse problem: it is on `availability` and in the
    // messages, and `parse_warnings` stays 0 because the parser did not have to skip or guess.
    assert_eq!(document.provenance.parse_warnings, 0);
    assert_eq!(document.provenance.endpoint, "player");
}

/// `microformat` is optional; the thumbnails it would have carried live on in `videoDetails`.
#[tokio::test]
async fn missing_microformat_falls_back_to_video_details_thumbnails() {
    let mut payload: Value = serde_json::from_str(&fixture(
        "recorded/player_probe_public_unplayable_2026-11-14.json",
    ))
    .expect("fixture is JSON");
    payload
        .as_object_mut()
        .expect("object")
        .remove("microformat");

    let provider = provider_with(&payload.to_string());
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Lenient)
            .await
            .expect("document"),
    );
    assert!(
        document.summary.published_at.is_none(),
        "no microformat, no publish date — invented dates would be fabrication"
    );
    assert_eq!(document.summary.thumbnails.len(), 5);
    assert!(
        document
            .summary
            .thumbnails
            .iter()
            .all(|thumbnail| thumbnail.url.starts_with("https://i.ytimg.com/")),
        "{:?}",
        document.summary.thumbnails
    );
}

/// Strictness does not turn a complete-but-unavailable document into an error: nothing was skipped.
#[tokio::test]
async fn live_unplayable_capture_is_accepted_in_strict_mode() {
    let provider = provider_with(&fixture(
        "recorded/player_probe_public_unplayable_2026-11-14.json",
    ));
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Strict)
            .await
            .expect("a complete document is not a parse problem"),
    );
    assert!(!document.availability.is_available());
}

/// Unknown fields — including inside `playabilityStatus` — must not change the outcome.
#[tokio::test]
async fn unknown_future_fields_are_ignored() {
    let mut payload: Value = serde_json::from_str(&fixture(
        "recorded/player_probe_public_unplayable_2026-11-14.json",
    ))
    .expect("fixture is JSON");
    payload["futureSection"] = serde_json::json!({"nested": [1, 2, 3]});
    payload["videoDetails"]["futureMetric"] = serde_json::json!("42");
    payload["playabilityStatus"]["futureFlag"] = serde_json::json!(true);

    let provider = provider_with(&payload.to_string());
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Lenient)
            .await
            .expect("document"),
    );
    assert_eq!(
        document
            .summary
            .view_count
            .as_ref()
            .map(|count| count.value),
        Some(1_836_979_877),
        "an unknown sibling field must not disturb a mapped one"
    );
    assert_eq!(document.summary.title, payload["videoDetails"]["title"]);
}

// --------------------------------------------------------------------------------------------
// Live capture: restricted answers with no document
// --------------------------------------------------------------------------------------------

/// A bot check is not a parse failure: upstream answered completely, it just refused the metadata.
#[tokio::test]
async fn live_login_required_capture_is_a_typed_unavailability() {
    let provider = provider_with(&fixture(
        "recorded/player_probe_login_required_2026-11-14.json",
    ));
    let error = video(&provider, "9Umm2MAKsMc", Strictness::Lenient)
        .await
        .expect_err("no document behind a bot check");
    let (reason, message) = unavailability(error);
    assert_eq!(reason, UnavailabilityReason::LoginRequired);
    assert_eq!(reason.as_str(), "login_required");
    assert!(
        message.contains("LOGIN_REQUIRED"),
        "the raw upstream status must stay observable: {message}"
    );
    assert!(message.contains("not a bot"), "{message}");
    // No session material, no tracking parameters, no body dump.
    assert!(!message.contains("visitorData"), "{message}");
    assert!(!message.contains("AIza"), "{message}");
}

/// A non-existent id is `ERROR` with no metadata: upstream does not distinguish "deleted" from
/// "never existed", so Vydra must not either — the reason stays `other`.
#[tokio::test]
async fn live_error_capture_is_a_typed_unavailability_without_inventing_a_reason() {
    let provider = provider_with(&fixture(
        "recorded/player_probe_error_unavailable_2026-11-14.json",
    ));
    let error = video(&provider, "AAAAAAAAAAA", Strictness::Lenient)
        .await
        .expect_err("no document for a non-existent id");
    let (reason, message) = unavailability(error);
    assert_eq!(reason, UnavailabilityReason::Other);
    assert_eq!(reason.as_str(), "other");
    assert!(message.contains("ERROR"), "{message}");
    assert!(message.contains("This video is unavailable"), "{message}");
}

/// `PLAYER_*` codes must not be confused: an unavailability is 409-shaped, never `provider_parse`.
#[tokio::test]
async fn restricted_answers_are_not_parse_errors() {
    for fixture_name in [
        "recorded/player_probe_login_required_2026-11-14.json",
        "recorded/player_probe_error_unavailable_2026-11-14.json",
    ] {
        let provider = provider_with(&fixture(fixture_name));
        let error = video(&provider, "9Umm2MAKsMc", Strictness::Lenient)
            .await
            .expect_err("no document");
        assert_eq!(error.code(), "unavailable", "{fixture_name}");
        assert!(!error.is_fallback_eligible(), "{fixture_name}");
    }
}

// --------------------------------------------------------------------------------------------
// Transformed capture: no verdict at all
// --------------------------------------------------------------------------------------------

/// A real document with `playabilityStatus` removed: the state is `Unknown`, never `Available`,
/// and strict mode refuses to guess.
#[tokio::test]
async fn transformed_capture_without_a_verdict_is_unknown() {
    let provider = provider_with(&fixture(
        "malformed/player_probe_without_playability_2026-11-14.json",
    ));
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Lenient)
            .await
            .expect("document"),
    );
    assert_eq!(document.availability, Availability::Unknown);
    assert!(!document.availability.is_available());
    assert!(
        document.provenance.parse_warnings >= 1,
        "the missing verdict must be counted"
    );

    let provider = provider_with(&fixture(
        "malformed/player_probe_without_playability_2026-11-14.json",
    ));
    let error = video(&provider, PUBLIC_ID, Strictness::Strict)
        .await
        .expect_err("strict mode must not accept a missing verdict");
    assert_eq!(error.code(), "provider_parse");
}

// --------------------------------------------------------------------------------------------
// Transport and envelope failures stay typed
// --------------------------------------------------------------------------------------------

/// A 5xx from the player endpoint is an upstream failure, not a document and not a parse error.
#[tokio::test]
async fn http_500_is_an_upstream_error() {
    let (provider, _) = provider_with_status(r#"{"error":{"code":500}}"#, 500);
    let error = video(&provider, PUBLIC_ID, Strictness::Lenient)
        .await
        .expect_err("5xx is not a document");
    assert_eq!(error.code(), "provider_upstream");
    assert!(error.to_string().contains("500"), "{error}");
}

/// A redirect is surfaced, never followed: a 3xx means one request and an error.
#[tokio::test]
async fn redirect_is_surfaced_not_followed() {
    let (provider, transport) = provider_with_status("", 302);
    let error = video(&provider, PUBLIC_ID, Strictness::Lenient)
        .await
        .expect_err("a redirect is not a document");
    assert_eq!(error.code(), "provider_upstream");
    assert_eq!(
        transport.calls().len(),
        2,
        "one bootstrap and one player request; a redirect must not be retried"
    );
}

/// A 200 with a truncated body is a parse failure, never a partially parsed document.
#[tokio::test]
async fn truncated_json_is_a_parse_error() {
    let provider = provider_with(r#"{"playabilityStatus":{"status":"OK"},"videoDetails":{"vid"#);
    let error = video(&provider, PUBLIC_ID, Strictness::Lenient)
        .await
        .expect_err("truncated JSON");
    assert_eq!(error.code(), "provider_parse");
}

/// A 200 with a completely different JSON document is a shape failure.
#[tokio::test]
async fn a_search_shaped_body_is_not_a_player_document() {
    let provider = provider_with(r#"{"contents":{"videoRenderer":{"videoId":"dQw4w9WgXcQ"}}}"#);
    let error = video(&provider, PUBLIC_ID, Strictness::Lenient)
        .await
        .expect_err("wrong shape");
    assert_eq!(error.code(), "provider_parse");
    assert!(error.to_string().contains("videoDetails"), "{error}");
}

// --------------------------------------------------------------------------------------------
// Phase 5B: microformat facts, live state, storyboards
// --------------------------------------------------------------------------------------------

/// The live capture carries facts the Phase 5A mapper did not read at all: `likeCount`, `uploadDate`,
/// `isUnlisted`/`isFamilySafe`, and a `lengthSeconds` that **disagrees** with `videoDetails`.
///
/// The disagreement is the point. `videoDetails.lengthSeconds` is `213` and
/// `microformat…lengthSeconds` is `214` in the same response; the document reports 213 because
/// `videoDetails` is primary, and says so in `evidence.conflicts` instead of hiding the other value.
#[tokio::test]
async fn live_capture_maps_the_microformat_facts_and_records_the_disagreement() {
    let provider = provider_with(&fixture(
        "recorded/player_probe_public_unplayable_2026-11-14.json",
    ));
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Lenient)
            .await
            .expect("document"),
    );

    // `likeCount` is a plain integer upstream and has exactly one source.
    let like_count = document.like_count.expect("like count");
    assert_eq!(like_count.value, 19_503_628);
    assert_eq!(
        like_count.precision,
        vydra_domain::Precision::Exact,
        "an integer field is not an estimate"
    );

    // Publication keeps the publisher's offset *as data* while the instant stays UTC.
    let publication = document.publication.expect("publication");
    assert_eq!(publication.upstream_text, "2009-10-24T23:57:33-07:00");
    assert_eq!(publication.upstream_offset_seconds, -25_200);
    assert_eq!(publication.published_at.offset(), UtcOffset::UTC);
    assert_eq!(publication.uploaded_at, Some(publication.published_at));

    // Content facts from the microformat.
    assert_eq!(document.unlisted, Some(false));
    assert_eq!(document.family_safe, Some(true));

    // Both sections are present, and the duration disagreement is recorded rather than averaged.
    assert!(document.evidence.has_section(FieldSource::VideoDetails));
    assert!(document.evidence.has_section(FieldSource::Microformat));
    let conflict = document
        .evidence
        .conflicts
        .iter()
        .find(|conflict| conflict.field == "duration_seconds")
        .expect("the capture disagrees about duration");
    assert_eq!(conflict.kept_value, "213");
    assert_eq!(conflict.rejected_value, "214");
    assert_eq!(document.summary.duration_seconds, Some(213));

    // This answer has no `streamingData`, no `captions` and no `storyboards`: absent, not invented.
    assert!(document.formats.is_empty());
    assert!(document.caption_tracks.is_empty());
    assert!(document.storyboards.is_none());

    // A recorded disagreement is a warning, not a counted loss: nothing was dropped or guessed.
    assert_eq!(document.provenance.parse_warnings, 0);
}

/// The synthetic fixture is the only place a *complete* `OK` response exists in this repository, and it
/// is explicitly synthetic (`synthetic/player_live_broadcast_storyboards.meta.json`). It exercises the
/// sections no capture carries together, not upstream's current behaviour.
#[tokio::test]
async fn synthetic_ok_capture_maps_live_state_storyboards_formats_and_captions() {
    let provider = provider_with(&fixture("synthetic/player_live_broadcast_storyboards.json"));
    let document = document(
        video(&provider, PUBLIC_ID, Strictness::Strict)
            .await
            .expect("a clean synthetic document, accepted even in strict mode"),
    );

    // An `OK` verdict on a live broadcast is `live`, not `available`: the two cache differently.
    assert_eq!(document.availability, Availability::Live);
    assert_eq!(document.summary.live_state, Some(LiveState::Live));
    let broadcast = document.broadcast.as_ref().expect("broadcast window");
    assert_eq!(broadcast.live_now, Some(true));
    assert_eq!(
        broadcast.start_at.map(|start| start.offset()),
        Some(UtcOffset::UTC)
    );

    // `lengthSeconds: "0"` is a value, and it must stay distinguishable from "not reported".
    assert_eq!(document.summary.duration_seconds, Some(0));
    assert_eq!(
        document.like_count.as_ref().map(|count| count.value),
        Some(99)
    );

    // Storyboards: geometry only.
    let storyboard = document.storyboards.as_ref().expect("storyboard geometry");
    assert!(storyboard.url_template_present);
    assert_eq!(storyboard.levels.len(), 2);
    assert_eq!(storyboard.levels[0].width, 48);
    assert_eq!(storyboard.levels[1].name.as_deref(), Some("M$M"));
    assert!(document.evidence.has_section(FieldSource::Storyboards));

    assert_eq!(document.formats.len(), 3, "2 adaptive + 1 muxed");
    assert_eq!(
        document.formats[0].delivery,
        vydra_domain::DeliveryKind::SignatureProtected
    );
    assert_eq!(document.caption_tracks.len(), 1);
    assert_eq!(document.caption_tracks[0].language_code, "en");

    // Nothing signed, nothing URL-shaped, nothing that could be mistaken for a working media link.
    let json = serde_json::to_string(&document).expect("serialize");
    for secret in ["SYNTHETICSIG", "example.invalid", "sqp="] {
        assert!(!json.contains(secret), "{secret} leaked into the document");
    }
}
