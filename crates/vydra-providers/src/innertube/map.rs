//! Mapping: InnerTube JSON → domain models.
//!
//! Search results are mapped from `serde_json::Value` with explicit helpers rather than a full DTO
//! mirror: renderer shapes vary per surface (search / grid / shelf) and a mirror would break on every
//! permutation (ADR-002). The player response has a stable top-level shape, so it is typed.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vydra_common::text::{is_abbreviated_count, parse_clock_duration, parse_view_count};
use vydra_common::{ChannelId, PlaylistId, VideoId};
use vydra_domain::{
    Availability, CaptionKind, CaptionTrack, Chapter, Count, DeliveryKind, LiveState,
    MediaFormatInfo, PlaylistSummary, Provenance, SearchItem, SearchResults, Thumbnail,
    VideoDocument, VideoSummary,
};
use vydra_errors::{ProviderError, UnavailabilityReason};

use crate::parse::{ParseReport, Renderer, find_continuation, number_of, text_of, thumbnails_of};

fn first_text(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(text_of) {
            return Some(text);
        }
    }
    None
}

fn count_from_text(raw: &str) -> Option<Count> {
    let value = parse_view_count(raw)?;
    if is_abbreviated_count(raw) {
        Some(Count::approximate(value, raw))
    } else {
        Some(Count::exact(value))
    }
}

/// Exact numeric fields win over localized text (ADR-016).
fn count_from_fields(value: &Value, numeric_key: &str, text_keys: &[&str]) -> Option<Count> {
    if let Some(number) = value.get(numeric_key).and_then(number_of) {
        return Some(Count::exact(number));
    }
    for key in text_keys {
        if let Some(count) = value
            .get(*key)
            .and_then(text_of)
            .and_then(|text| count_from_text(&text))
        {
            return Some(count);
        }
    }
    None
}

fn live_state_of(renderer: &Value) -> Option<LiveState> {
    let overlays = renderer.get("thumbnailOverlays")?.as_array()?;
    for overlay in overlays {
        let status = overlay
            .get("thumbnailOverlayTimeStatusRenderer")
            .and_then(|time| time.get("style"))
            .and_then(Value::as_str);
        match status {
            Some("LIVE") => return Some(LiveState::Live),
            Some("UPCOMING") => return Some(LiveState::Upcoming),
            _ => {}
        }
    }
    // Badge based detection ("LIVE NOW" badge label) as a secondary signal.
    if let Some(badges) = renderer.get("badges").and_then(Value::as_array) {
        for badge in badges {
            if let Some(label) = badge
                .get("metadataBadgeRenderer")
                .and_then(|b| b.get("style"))
                .and_then(Value::as_str)
                && label.eq_ignore_ascii_case("LIVE")
            {
                return Some(LiveState::Live);
            }
        }
    }
    None
}

/// Channel id from `browseEndpoint.browseId` on the owner/byline runs.
fn channel_id_of(renderer: &Value, keys: &[&str]) -> Option<ChannelId> {
    for key in keys {
        let Some(field) = renderer.get(*key) else {
            continue;
        };
        let Some(runs) = field.get("runs").and_then(Value::as_array) else {
            continue;
        };
        for run in runs {
            if let Some(id) = run
                .get("navigationEndpoint")
                .and_then(|endpoint| endpoint.get("browseEndpoint"))
                .and_then(|browse| browse.get("browseId"))
                .and_then(Value::as_str)
                && let Ok(channel_id) = ChannelId::parse(id)
            {
                return Some(channel_id);
            }
        }
    }
    None
}

fn video_summary_from_renderer(renderer: &Value, report: &mut ParseReport) -> Option<VideoSummary> {
    let raw_id = renderer.get("videoId").and_then(Value::as_str);
    let Some(raw_id) = raw_id else {
        report.skipped_item("video_without_id");
        return None;
    };
    let id = match VideoId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            report.skipped_item("video_id_invalid");
            return None;
        }
    };

    let title = first_text(renderer, &["title", "headline"]);
    let Some(title) = title else {
        report.missing_field("videoRenderer", "title");
        return None;
    };

    let duration = renderer
        .get("lengthText")
        .and_then(text_of)
        .and_then(|text| parse_clock_duration(&text));

    let view_count = count_from_fields(
        renderer,
        "viewCount",
        &["viewCountText", "shortViewCountText"],
    );

    let published_text = first_text(renderer, &["publishedTimeText"]);

    let mut thumbnails = renderer
        .get("thumbnail")
        .map(thumbnails_of)
        .unwrap_or_default();
    if thumbnails.is_empty()
        && let Some(url) = renderer
            .get("thumbnail")
            .and_then(|t| t.as_str())
            .and_then(vydra_common::text::normalize_thumbnail_url)
    {
        thumbnails.push(Thumbnail {
            url,
            width: None,
            height: None,
        });
    }
    // Smallest → largest: providers are not guaranteed to be sorted.
    thumbnails.sort_by_key(|thumb| thumb.width.unwrap_or(0));

    let badges = renderer
        .get("badges")
        .and_then(Value::as_array)
        .map(|badges| {
            badges
                .iter()
                .filter_map(|badge| {
                    let metadata = badge.get("metadataBadgeRenderer")?;
                    metadata
                        .get("label")
                        .and_then(Value::as_str)
                        .or_else(|| metadata.get("style").and_then(Value::as_str))
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(VideoSummary {
        id,
        title,
        channel_id: channel_id_of(
            renderer,
            &["ownerText", "longBylineText", "shortBylineText"],
        ),
        channel_title: first_text(
            renderer,
            &["ownerText", "longBylineText", "shortBylineText"],
        ),
        duration_seconds: duration,
        view_count,
        published_text,
        published_at: None,
        thumbnails,
        live_state: live_state_of(renderer),
        badges,
    })
}

fn channel_summary_from_renderer(
    renderer: &Value,
    report: &mut ParseReport,
) -> Option<vydra_domain::ChannelSummary> {
    let Some(raw_id) = renderer.get("channelId").and_then(Value::as_str) else {
        report.skipped_item("channel_without_id");
        return None;
    };
    let channel_id = match ChannelId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            report.skipped_item("channel_id_invalid");
            return None;
        }
    };
    let Some(title) = first_text(renderer, &["title"]) else {
        report.missing_field("channelRenderer", "title");
        return None;
    };

    // Handles are only trustworthy when the provider exposes the canonical channel URL.
    let handle = renderer
        .get("navigationEndpoint")
        .and_then(|endpoint| endpoint.get("browseEndpoint"))
        .and_then(|browse| browse.get("canonicalBaseUrl"))
        .and_then(Value::as_str)
        .and_then(|url| url.strip_prefix('/'))
        .filter(|path| path.starts_with('@'))
        .map(|path| path.trim_start_matches('@').to_owned());

    Some(vydra_domain::ChannelSummary {
        id: channel_id,
        title,
        handle,
        description: first_text(renderer, &["descriptionSnippet"]),
        thumbnails: renderer
            .get("thumbnail")
            .map(thumbnails_of)
            .unwrap_or_default(),
        subscriber_count: renderer
            .get("subscriberCountText")
            .and_then(text_of)
            .and_then(|text| count_from_text(&text)),
        video_count: renderer
            .get("videoCountText")
            .and_then(text_of)
            .and_then(|text| count_from_text(&text)),
    })
}

fn playlist_summary_from_renderer(
    renderer: &Value,
    report: &mut ParseReport,
) -> Option<PlaylistSummary> {
    let Some(raw_id) = renderer.get("playlistId").and_then(Value::as_str) else {
        report.skipped_item("playlist_without_id");
        return None;
    };
    let playlist_id = match PlaylistId::parse(raw_id) {
        Ok(id) => id,
        Err(_) => {
            report.skipped_item("playlist_id_invalid");
            return None;
        }
    };
    let Some(title) = first_text(renderer, &["title"]) else {
        report.missing_field("playlistRenderer", "title");
        return None;
    };

    Some(PlaylistSummary {
        id: playlist_id,
        title,
        channel_id: channel_id_of(renderer, &["shortBylineText", "longBylineText"]),
        channel_title: first_text(renderer, &["shortBylineText", "longBylineText"]),
        description: None,
        // `videoCount` is the plain field ("20"); `videoCountText` is the displayed form
        // ("20 videos" / localized runs). Both are read so a response that carries only the
        // rendered string does not lose the count; none of the Phase 4.3 captures contained a
        // playlist, so this order is reference-backed, not capture-backed.
        item_count: ["videoCount", "videoCountText"].iter().find_map(|key| {
            renderer
                .get(*key)
                .and_then(text_of)
                .and_then(|text| count_from_text(&text))
        }),
        thumbnails: renderer
            .get("thumbnail")
            .map(thumbnails_of)
            .unwrap_or_default(),
    })
}

/// Map one renderer into a [`SearchItem`], reporting skips instead of failing the page.
pub fn search_item_from_renderer(
    renderer: &Renderer<'_>,
    report: &mut ParseReport,
) -> Option<SearchItem> {
    match renderer.key {
        "videoRenderer"
        | "compactVideoRenderer"
        | "gridVideoRenderer"
        | "playlistVideoRenderer"
        | "playlistPanelVideoRenderer"
        | "channelVideoPlayerRenderer" => {
            video_summary_from_renderer(renderer.value, report).map(SearchItem::Video)
        }
        "channelRenderer" | "gridChannelRenderer" => {
            channel_summary_from_renderer(renderer.value, report).map(SearchItem::Channel)
        }
        "playlistRenderer" | "gridPlaylistRenderer" => {
            playlist_summary_from_renderer(renderer.value, report).map(SearchItem::Playlist)
        }
        other => {
            // `collect_renderers` only hands over keys from `KNOWN_RENDERERS`, i.e. shapes that are
            // *known to be content* — and every one that lands here has no mapping yet. Counting it
            // as `unknown_renderer` (the chrome counter) hid 11 real items on the Phase 4.3 capture;
            // `unmapped_result` is the counter that says "a result was dropped".
            report.unmapped_result(other);
            None
        }
    }
}

/// Build [`SearchResults`] from a raw search/continuation response.
/// De-duplication key for a mapped search item: the entity kind plus its id.
fn item_key(item: &SearchItem) -> (u8, String) {
    match item {
        SearchItem::Video(video) => (0, video.id.as_str().to_owned()),
        SearchItem::Channel(channel) => (1, channel.id.as_str().to_owned()),
        SearchItem::Playlist(playlist) => (2, playlist.id.as_str().to_owned()),
    }
}

pub fn map_search(
    payload: &Value,
    renderers: Vec<Renderer<'_>>,
    report: &mut ParseReport,
    provenance: Provenance,
) -> SearchResults {
    let mut items = Vec::with_capacity(renderers.len());
    let mut seen: HashSet<(u8, String)> = HashSet::new();
    for renderer in &renderers {
        let Some(item) = search_item_from_renderer(renderer, report) else {
            continue;
        };
        // The same entity can appear twice on one page (a video in a shelf and again in the result
        // list, a playlist promoted at the top). The domain model carries no per-occurrence context,
        // so the second copy is information-free — and keeping it would make `limit` arithmetic and
        // the client's "append at the end" cursor assumption wrong. Counted, never silent.
        if !seen.insert(item_key(&item)) {
            report.skipped_item("duplicate_item");
            continue;
        }
        items.push(item);
    }

    let estimated_results = payload
        .get("estimatedResults")
        .and_then(number_of)
        .map(Count::exact);

    SearchResults {
        items,
        next_cursor: find_continuation(payload),
        estimated_results,
        provenance,
    }
}

// --------------------------------------------------------------------------------------------
// Player
// --------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponseDto {
    playability_status: Option<PlayabilityStatusDto>,
    video_details: Option<VideoDetailsDto>,
    microformat: Option<Value>,
    captions: Option<Value>,
    streaming_data: Option<StreamingDataDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayabilityStatusDto {
    status: Option<String>,
    reason: Option<String>,
    /// Upstream's own explanation, e.g. `[{"text": "This video is private"}]`. Used to *classify*
    /// the restriction and never copied into a document.
    messages: Option<Vec<Value>>,
    /// `errorScreen.playerErrorMessageRenderer`: the rendered explanation for `LOGIN_REQUIRED` and
    /// friends, which often carries the wording the `reason` field omits.
    error_screen: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetailsDto {
    video_id: Option<String>,
    title: Option<String>,
    length_seconds: Option<String>,
    view_count: Option<String>,
    author: Option<String>,
    channel_id: Option<String>,
    short_description: Option<String>,
    keywords: Option<Vec<String>>,
    is_live_content: Option<bool>,
    is_live_now: Option<bool>,
    is_upcoming: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingDataDto {
    formats: Option<Vec<Value>>,
    adaptive_formats: Option<Vec<Value>>,
}

/// The playability verdict, classified into Vydra's vocabulary.
///
/// The distinction that matters: `Ok` is the only state that may become a document, and
/// `Unrecognised` is never silently promoted to `Ok`. See `docs/research/PLAYER_RESPONSE_ANALYSIS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Playability {
    /// Upstream said `status: "OK"`.
    Ok,
    /// A named restriction this build can report.
    Restricted(UnavailabilityReason),
    /// A status or wording this build does not recognise. Observable, never success.
    Unrecognised,
    /// No verdict at all: `playabilityStatus` (or its `status` member) is absent.
    Missing,
}

impl Playability {
    /// The restriction this verdict names, for the error path when there is no document to return.
    fn restriction(self) -> Option<UnavailabilityReason> {
        match self {
            Self::Restricted(reason) => Some(reason),
            Self::Unrecognised => Some(UnavailabilityReason::Other),
            Self::Ok | Self::Missing => None,
        }
    }
}

/// Append every string value of a JSON fragment, bounded, so the wording can be matched without
/// copying it into any output.
fn collect_strings(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            for child in map.values() {
                collect_strings(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::String(text) => {
            if out.len() < 512 {
                out.push(' ');
                out.push_str(text);
            }
        }
        _ => {}
    }
}

/// Lowercased classification text: the `reason` field, then whatever `messages` and `errorScreen`
/// say. `LOGIN_REQUIRED` explains the age gate there rather than in `reason`.
fn playability_detail(status: &PlayabilityStatusDto) -> String {
    let mut text = String::new();
    if let Some(reason) = &status.reason {
        text.push_str(reason);
    }
    if let Some(messages) = &status.messages {
        for message in messages {
            collect_strings(message, &mut text);
        }
    }
    if let Some(screen) = &status.error_screen {
        collect_strings(screen, &mut text);
    }
    text.to_ascii_lowercase()
}

fn classify_playability(status: &PlayabilityStatusDto) -> Playability {
    match status.status.as_deref() {
        Some("OK") => Playability::Ok,
        Some(raw) => {
            let detail = playability_detail(status);
            let normalized = if detail.trim().is_empty() {
                None
            } else {
                Some(detail.as_str())
            };
            match unavailability_from_status(raw, normalized) {
                UnavailabilityReason::Other => Playability::Unrecognised,
                reason => Playability::Restricted(reason),
            }
        }
        None => Playability::Missing,
    }
}

/// Whole-word match, so `"the page needs to be reloaded"` is not an age restriction.
///
/// The live capture behind this is recorded in `docs/research/PLAYER_RESPONSE_ANALYSIS.md`: substring
/// matching on `"age"` (inside `"page"`) classified an unrelated `UNPLAYABLE` verdict as
/// `age_restricted`, i.e. it invented a restriction upstream never stated.
fn mentions(detail: &str, words: &[&str]) -> bool {
    detail
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| words.contains(&word))
}

/// Typed unavailability that keeps the raw upstream status in its message.
///
/// The message is a fixed phrase plus at most 200 printable characters of upstream's `reason`; the
/// response body itself is never quoted, so nothing session-scoped can reach a log or a response.
fn playability_error(status: &PlayabilityStatusDto, reason: UnavailabilityReason) -> ProviderError {
    let raw = status.status.as_deref().unwrap_or("absent");
    let detail: Option<String> = status.reason.as_deref().map(|reason| {
        reason
            .chars()
            .filter(|character| !character.is_control())
            .take(200)
            .collect()
    });
    let message = match detail {
        Some(detail) if !detail.trim().is_empty() => format!("playability {raw}: {detail}"),
        _ => format!("playability {raw}"),
    };
    tracing::warn!(
        playability.status = raw,
        playability.reason = reason.as_str(),
        "player reported a non-OK verdict"
    );
    ProviderError::Unavailable { reason, message }
}

fn unavailability_from_status(status: &str, detail: Option<&str>) -> UnavailabilityReason {
    let detail_lower = detail.unwrap_or_default().to_ascii_lowercase();
    // Wording wins over the status code: several restrictions share a status, and only the wording
    // distinguishes them. Matching is whole-word (or a phrase), never a bare substring.
    if mentions(&detail_lower, &["private"]) {
        return UnavailabilityReason::Private;
    }
    if mentions(&detail_lower, &["members", "membership"]) {
        return UnavailabilityReason::MembersOnly;
    }
    if mentions(&detail_lower, &["country", "region"]) {
        return UnavailabilityReason::GeoRestricted;
    }
    if mentions(&detail_lower, &["age"]) {
        return UnavailabilityReason::AgeRestricted;
    }
    if mentions(&detail_lower, &["copyright"]) {
        return UnavailabilityReason::CopyrightBlocked;
    }
    if mentions(&detail_lower, &["drm"]) || detail_lower.contains("protected content") {
        return UnavailabilityReason::DrmProtected;
    }
    if mentions(&detail_lower, &["deleted", "removed"])
        || detail_lower.contains("no longer available")
    {
        return UnavailabilityReason::Deleted;
    }
    if mentions(&detail_lower, &["premiere"]) || detail_lower.contains("live stream") {
        return UnavailabilityReason::NotBroadcasting;
    }
    match status {
        "LOGIN_REQUIRED" | "AGE_CHECK_REQUIRED" | "AGE_VERIFICATION_REQUIRED" => {
            UnavailabilityReason::LoginRequired
        }
        "LIVE_STREAM_OFFLINE" => UnavailabilityReason::NotBroadcasting,
        _ => UnavailabilityReason::Other,
    }
}

fn parse_chapters(description: &str) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = Vec::new();
    for line in description.lines() {
        let line = line.trim();
        // Accept both `0:00 Title` and `0:00 - Title` / `00:00:00 Title`.
        let Some((timestamp, rest)) = line.split_once(' ') else {
            continue;
        };
        if !timestamp.chars().all(|c| c.is_ascii_digit() || c == ':') {
            continue;
        }
        let Some(start) = parse_clock_duration(timestamp) else {
            continue;
        };
        let title = rest.trim_start_matches(['-', '–', '|', ' ']).trim();
        if title.is_empty() {
            continue;
        }
        chapters.push(Chapter {
            title: title.to_owned(),
            start_seconds: start,
            end_seconds: None,
            thumbnail: None,
        });
    }
    // A single marker is not a chapter list.
    if chapters.len() < 2 {
        return Vec::new();
    }
    chapters.sort_by_key(|chapter| chapter.start_seconds);
    for index in 0..chapters.len().saturating_sub(1) {
        chapters[index].end_seconds = Some(chapters[index + 1].start_seconds);
    }
    chapters
}

fn caption_tracks_from(value: &Value) -> Vec<CaptionTrack> {
    let tracks = value
        .get("playerCaptionsTracklistRenderer")
        .and_then(|renderer| renderer.get("captionTracks"))
        .and_then(Value::as_array);

    let Some(tracks) = tracks else {
        return Vec::new();
    };

    tracks
        .iter()
        .filter_map(|track| {
            let language_code = track
                .get("languageCode")
                .and_then(Value::as_str)?
                .to_owned();
            let kind = match track.get("kind").and_then(Value::as_str) {
                Some("asr") => CaptionKind::Automatic,
                None => CaptionKind::Manual,
                Some(_) => CaptionKind::Unknown,
            };
            let translation_languages = track
                .get("translationLanguages")
                .and_then(Value::as_array)
                .map(|languages| {
                    languages
                        .iter()
                        .filter_map(|language| {
                            language
                                .get("languageCode")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(CaptionTrack {
                language_code,
                name: track.get("name").and_then(text_of).or_else(|| {
                    track
                        .get("vssId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
                kind,
                is_translatable: track
                    .get("isTranslatable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                base_url_present: track.get("baseUrl").is_some(),
                translation_languages,
            })
        })
        .collect()
}

/// Map one format entry. **No URL is ever copied into the domain model** (ADR-006).
fn media_format_from(value: &Value) -> Option<MediaFormatInfo> {
    let mime_type = value.get("mimeType").and_then(Value::as_str);
    let itag = value
        .get("itag")
        .and_then(number_of)
        .and_then(|v| u32::try_from(v).ok());
    let has_drm = value
        .get("drmFamilies")
        .map(|drm| !drm.is_null())
        .unwrap_or(false);

    let delivery = if has_drm {
        DeliveryKind::Drm
    } else if value.get("url").is_some() {
        DeliveryKind::Direct
    } else if value.get("signatureCipher").is_some() || value.get("cipher").is_some() {
        DeliveryKind::SignatureProtected
    } else {
        DeliveryKind::Unknown
    };

    let audio_only = mime_type
        .map(|mime| mime.starts_with("audio/"))
        .unwrap_or(false);

    Some(MediaFormatInfo {
        itag,
        mime_type: mime_type.map(str::to_owned),
        codecs: value
            .get("codecs")
            .and_then(Value::as_str)
            .map(str::to_owned),
        bitrate: value.get("bitrate").and_then(number_of),
        width: value
            .get("width")
            .and_then(number_of)
            .and_then(|v| u32::try_from(v).ok()),
        height: value
            .get("height")
            .and_then(number_of)
            .and_then(|v| u32::try_from(v).ok()),
        fps: value
            .get("fps")
            .and_then(number_of)
            .and_then(|v| u32::try_from(v).ok()),
        quality_label: value
            .get("qualityLabel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        audio_quality: value
            .get("audioQuality")
            .and_then(Value::as_str)
            .map(str::to_owned),
        content_length: value.get("contentLength").and_then(number_of),
        approx_duration_ms: value.get("approxDurationMs").and_then(number_of),
        audio_only,
        has_drm,
        delivery,
    })
}

/// Build a [`VideoDocument`] from a player response.
pub fn map_player(
    payload: &Value,
    expected_id: &VideoId,
    report: &mut ParseReport,
    provenance: Provenance,
) -> Result<VideoDocument, ProviderError> {
    let dto: PlayerResponseDto =
        serde_json::from_value(payload.clone()).map_err(|err| ProviderError::Parse {
            message: format!("player response shape: {err}"),
        })?;

    // The verdict decides what may be returned, so it is read before the payload is required to
    // carry video details: a restricted answer legitimately arrives without `videoDetails`.
    //
    // Evidence (Phase 5A, capture `docs/research/data/player-probe-43549141233`): an `UNPLAYABLE`
    // answer for a public video still carries `videoDetails` and `microformat`, while a bot check
    // (`LOGIN_REQUIRED`) and a non-existent id (`ERROR`) carry neither. Two shapes, two outcomes:
    //
    //   * verdict not OK, metadata present -> a document with `Availability::Unavailable { reason }`
    //     (the API serves the metadata it did receive, with an explicit state);
    //   * verdict not OK, metadata absent  -> `ProviderError::Unavailable`, i.e. a 409
    //     `content_unavailable` carrying the reason (`docs/architecture/API_CONTRACTS.md` §1, ADR-016).
    //
    // Neither outcome may claim `Available`, and an unrecognised verdict is never silent.
    let playability = dto.playability_status.as_ref();
    let raw_status = playability.and_then(|status| status.status.as_deref());
    let verdict = match playability {
        Some(status) => classify_playability(status),
        None => Playability::Missing,
    };
    if verdict == Playability::Missing {
        report.missing_field("playabilityStatus", "status");
    } else if verdict != Playability::Ok
        && let Some(status) = raw_status
    {
        // Recorded for every non-OK verdict, so an unknown or future status stays observable even
        // when the payload also carries a usable metadata document.
        report.warn(format!("playability:{status}"));
    }

    let details = match dto.video_details {
        Some(details) => details,
        None => {
            if let Some(reason) = verdict.restriction()
                && let Some(status) = playability
            {
                return Err(playability_error(status, reason));
            }
            return Err(ProviderError::Parse {
                message: "player response without videoDetails".into(),
            });
        }
    };

    // Identity is required, not optional: without `videoDetails.videoId` the document cannot be
    // attributed to the requested id, so this is a parse failure rather than a nullable field.
    let upstream_id = details
        .video_id
        .as_deref()
        .ok_or_else(|| ProviderError::Parse {
            message: "player response without videoDetails.videoId".into(),
        })?;
    if upstream_id != expected_id.as_str() {
        // Documented upstream behaviour: an outdated client can answer with another video
        // (`docs/research/EXTRACTION_METHODS.md` §13).
        return Err(ProviderError::Parse {
            message: format!("wrong result: requested {expected_id}, got {upstream_id}"),
        });
    }

    let availability = match verdict {
        Playability::Ok => Availability::Available,
        Playability::Restricted(reason) => Availability::Unavailable { reason },
        Playability::Unrecognised => Availability::Unavailable {
            reason: UnavailabilityReason::Other,
        },
        // The document keeps the data it did receive, but never claims availability: `Unknown` is not
        // a synonym for `Available`, is never cached and is never playable.
        Playability::Missing => Availability::Unknown,
    };

    let title = details.title.unwrap_or_default();
    if title.trim().is_empty() {
        report.missing_field("videoDetails", "title");
    }

    let duration_seconds = details
        .length_seconds
        .as_deref()
        .and_then(|value| value.parse::<u32>().ok());

    let view_count = details
        .view_count
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Count::exact);

    let channel_id = details
        .channel_id
        .as_deref()
        .and_then(|value| ChannelId::parse(value).ok());

    let live_state = if details.is_live_now == Some(true) {
        Some(LiveState::Live)
    } else if details.is_upcoming == Some(true) {
        Some(LiveState::Upcoming)
    } else if details.is_live_content == Some(true) {
        Some(LiveState::NotLive)
    } else {
        None
    };

    let microformat = dto.microformat.clone();
    let published_at = microformat
        .as_ref()
        .and_then(|micro| {
            micro
                .get("playerMicroformatRenderer")
                .or_else(|| micro.get("microformat"))
                .and_then(|renderer| renderer.get("publishDate"))
                .and_then(Value::as_str)
        })
        .and_then(|date| OffsetDateTime::parse(date, &Rfc3339).ok());

    let category = microformat
        .as_ref()
        .and_then(|micro| micro.get("playerMicroformatRenderer"))
        .and_then(|renderer| renderer.get("category"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let thumbnails = microformat
        .as_ref()
        .and_then(|micro| micro.get("playerMicroformatRenderer"))
        .and_then(|renderer| renderer.get("thumbnail"))
        .map(thumbnails_of)
        .unwrap_or_default();

    let description = details.short_description.clone();
    let chapters = description
        .as_deref()
        .map(parse_chapters)
        .unwrap_or_default();

    let caption_tracks = dto
        .captions
        .as_ref()
        .map(caption_tracks_from)
        .unwrap_or_default();

    let mut formats: Vec<MediaFormatInfo> = dto
        .streaming_data
        .as_ref()
        .and_then(|data| data.adaptive_formats.as_ref())
        .map(|formats| formats.iter().filter_map(media_format_from).collect())
        .unwrap_or_default();
    if let Some(muxed) = dto
        .streaming_data
        .as_ref()
        .and_then(|data| data.formats.as_ref())
    {
        formats.extend(muxed.iter().filter_map(media_format_from));
    }
    if formats.is_empty() && availability.is_available() {
        report.warn("no_formats");
    }

    Ok(VideoDocument {
        summary: VideoSummary {
            id: expected_id.clone(),
            title,
            channel_id,
            channel_title: details.author,
            duration_seconds,
            view_count,
            published_text: None,
            published_at,
            thumbnails,
            live_state,
            badges: Vec::new(),
        },
        availability,
        description,
        category,
        keywords: details.keywords.unwrap_or_default(),
        like_count: None,
        comment_count: None,
        chapters,
        caption_tracks,
        formats,
        provenance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vydra_domain::Precision;

    use crate::Strictness;

    fn provenance() -> Provenance {
        Provenance {
            provider: "test".into(),
            endpoint: "search".into(),
            client: Some("web".into()),
            fetched_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts"),
            cached: false,
            parse_warnings: 0,
            sources: Vec::new(),
        }
    }

    #[test]
    fn player_without_playability_never_claims_an_available_document() {
        let payload = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "A video", "lengthSeconds": "10"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");

        // Phase 5A: the document keeps the data it did receive, but a missing verdict is reported as
        // `Unknown` — the old default (`Available`) manufactured a success the response never
        // claimed, and the API would have cached and served it as playable.
        assert_eq!(document.availability, Availability::Unknown);
        assert!(!document.availability.is_available());
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message.contains("playabilityStatus.status")),
            "messages: {:?}",
            report.messages()
        );
        assert!(report.count() >= 1);
        assert!(report.check(Strictness::Strict).is_err());
    }

    #[test]
    fn search_maps_mixed_items_and_skips_broken_ones() {
        let payload = json!({
            "estimatedResults": 1000,
            "contents": [
                {"videoRenderer": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": {"runs": [{"text": "A song"}]},
                    "ownerText": {"runs": [{"text": "Rick", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCBv7HEHvY6Y0a0dS0R5mQ0w"}}}]},
                    "lengthText": {"simpleText": "3:33"},
                    "viewCountText": {"simpleText": "1.2B views"},
                    "publishedTimeText": {"simpleText": "15 years ago"},
                    "thumbnail": {"thumbnails": [
                        {"url": "//i.ytimg.com/vi/x/low.jpg", "width": 120, "height": 90},
                        {"url": "//i.ytimg.com/vi/x/high.jpg", "width": 480, "height": 360}
                    ]},
                    "badges": [{"metadataBadgeRenderer": {"label": "4K"}}]
                }},
                {"channelRenderer": {
                    "channelId": "UCBv7HEHvY6Y0a0dS0R5mQ0w",
                    "title": {"simpleText": "A channel"},
                    "subscriberCountText": {"simpleText": "1.5M subscribers"},
                    "videoCountText": {"simpleText": "120 videos"}
                }},
                {"playlistRenderer": {
                    "playlistId": "PLabcdefghijkl",
                    "title": {"simpleText": "A playlist"},
                    "videoCount": "12",
                    "shortBylineText": {"runs": [{"text": "Rick"}]}
                }},
                {"videoRenderer": {"title": {"simpleText": "missing id"}}},
                {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "NEXT"}}}}
            ]
        });

        let mut report = ParseReport::new();
        let renderers = crate::parse::collect_renderers(&payload, &mut report);
        let results = map_search(&payload, renderers, &mut report, provenance());

        assert_eq!(results.items.len(), 3);
        assert_eq!(results.next_cursor.as_deref(), Some("NEXT"));
        assert_eq!(results.estimated_results.map(|c| c.value), Some(1000));
        assert_eq!(report.skipped_items(), 1);

        match &results.items[0] {
            SearchItem::Video(video) => {
                assert_eq!(video.duration_seconds, Some(213));
                assert_eq!(
                    video.view_count.as_ref().map(|c| c.value),
                    Some(1_200_000_000)
                );
                assert_eq!(
                    video.view_count.as_ref().map(|c| c.precision),
                    Some(Precision::Approximate)
                );
                assert_eq!(video.thumbnails.len(), 2);
                assert_eq!(video.thumbnails[1].width, Some(480));
                assert!(video.thumbnails[0].url.starts_with("https://"));
                assert_eq!(video.badges, vec!["4K".to_owned()]);
                assert!(video.channel_id.is_some());
            }
            other => panic!("unexpected first item: {other:?}"),
        }
    }

    #[test]
    fn chapters_are_extracted_from_description() {
        let description = "Intro\n0:00 Start\n1:30 Middle\n2:45:10 End\nThanks";
        let chapters = parse_chapters(description);
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].title, "Start");
        assert_eq!(chapters[0].start_seconds, 0);
        assert_eq!(chapters[0].end_seconds, Some(90));
        assert_eq!(chapters[2].start_seconds, 9910);
    }

    #[test]
    fn single_timestamp_is_not_a_chapter_list() {
        assert!(parse_chapters("0:00 just one").is_empty());
    }

    #[test]
    fn formats_never_expose_urls_and_report_protection() {
        let payload = json!({
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "Song",
                "lengthSeconds": "213",
                "viewCount": "100",
                "channelId": "UCBv7HEHvY6Y0a0dS0R5mQ0w"
            },
            "playabilityStatus": {"status": "OK"},
            "streamingData": {"adaptiveFormats": [
                {"itag": 137, "mimeType": "video/mp4", "bitrate": 4000000, "width": 1920, "height": 1080,
                 "signatureCipher": {"s": "x", "sp": "sig", "url": "https://example.invalid"}},
                {"itag": 140, "mimeType": "audio/mp4", "bitrate": 128000, "url": "https://example.invalid"}
            ]}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("mapped");

        assert_eq!(document.formats.len(), 2);
        assert_eq!(
            document.formats[0].delivery,
            DeliveryKind::SignatureProtected
        );
        assert_eq!(document.formats[1].delivery, DeliveryKind::Direct);
        assert!(document.formats[1].audio_only);
        // The domain type has no url field at all: the test verifies the contract structurally.
        let json = serde_json::to_string(&document).expect("serialize");
        assert!(!json.contains("example.invalid"));
    }

    /// `UnavailabilityReason` for one payload, via the full mapping path.
    ///
    /// Only used for payloads *without* `videoDetails`, which is the shape that must become a typed
    /// error rather than a document.
    fn mapped_unavailability(payload: &serde_json::Value) -> UnavailabilityReason {
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        match map_player(payload, &id, &mut report, provenance()) {
            Err(ProviderError::Unavailable { reason, .. }) => reason,
            other => panic!("expected a typed unavailability, got {other:?}"),
        }
    }

    #[test]
    fn restriction_without_metadata_is_a_typed_unavailability_with_the_raw_status() {
        // No `videoDetails`: the verdict is the whole answer, so it becomes a 409 with the reason.
        let payload = json!({
            "playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "This video is private"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let error = map_player(&payload, &id, &mut report, provenance())
            .expect_err("a restricted response is not a document");

        assert_eq!(error.code(), "unavailable");
        let ProviderError::Unavailable { reason, message } = &error else {
            panic!("expected an unavailability, got {error:?}");
        };
        assert_eq!(*reason, UnavailabilityReason::Private);
        assert!(
            message.contains("LOGIN_REQUIRED"),
            "the raw upstream status must stay observable: {message}"
        );
    }

    #[test]
    fn restriction_with_metadata_keeps_the_document_but_never_claims_availability() {
        // The live `UNPLAYABLE` capture for a public video carries `videoDetails` and `microformat`;
        // dropping that verified metadata would be as wrong as claiming the video is playable.
        let payload = json!({
            "playabilityStatus": {"status": "UNPLAYABLE", "reason": "The page needs to be reloaded."},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "A video", "lengthSeconds": "213"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");

        // "The page needs to be reloaded." contains "age" inside "page": substring matching used to
        // turn this into `age_restricted`, inventing a restriction upstream never stated.
        assert_eq!(
            document.availability,
            Availability::Unavailable {
                reason: UnavailabilityReason::Other
            }
        );
        assert!(!document.availability.is_available());
        assert_eq!(document.summary.title, "A video");
        assert_eq!(document.summary.duration_seconds, Some(213));
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message.contains("playability:UNPLAYABLE")),
            "a non-OK verdict must be observable: {:?}",
            report.messages()
        );
    }

    #[test]
    fn an_unrecognised_playability_status_is_never_success() {
        // With metadata: the document is returned, but the state is unavailable and the warning names
        // the raw status. Without metadata: a typed error. Neither is `Available`.
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let with_details = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t"},
            "playabilityStatus": {"status": "SOME_FUTURE_STATUS", "reason": "Unknown verdict"}
        });
        let mut report = ParseReport::new();
        let document = map_player(&with_details, &id, &mut report, provenance()).expect("document");
        assert_eq!(
            document.availability,
            Availability::Unavailable {
                reason: UnavailabilityReason::Other
            }
        );
        assert!(!document.availability.is_available());
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message.contains("SOME_FUTURE_STATUS")),
            "messages: {:?}",
            report.messages()
        );

        let without_details = json!({
            "playabilityStatus": {"status": "SOME_FUTURE_STATUS", "reason": "Unknown verdict"}
        });
        let mut report = ParseReport::new();
        let error = map_player(&without_details, &id, &mut report, provenance())
            .expect_err("an unknown verdict without metadata must not become a document");
        let ProviderError::Unavailable { reason, message } = &error else {
            panic!("expected an unavailability, got {error:?}");
        };
        assert_eq!(*reason, UnavailabilityReason::Other);
        assert!(message.contains("SOME_FUTURE_STATUS"), "{message}");
    }

    #[test]
    fn playability_status_without_a_status_member_is_not_success() {
        let payload = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t"},
            "playabilityStatus": {"reason": "something"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");
        assert_eq!(document.availability, Availability::Unknown);
        assert!(!document.availability.is_available());
        assert!(report.count() >= 1);
    }

    #[test]
    fn restriction_wording_selects_the_reason_and_only_whole_words_count() {
        // One status, several content facts: only the wording separates them.
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "Sign in to confirm you're not a bot"}
            })),
            UnavailabilityReason::LoginRequired
        );
        // The age gate explains itself in `messages`, not in `reason`.
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {
                    "status": "LOGIN_REQUIRED",
                    "messages": [{"text": "Sign in to confirm your age"}]
                }
            })),
            UnavailabilityReason::AgeRestricted
        );
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {
                    "status": "UNPLAYABLE",
                    "reason": "The uploader has not made this video available in your country"
                }
            })),
            UnavailabilityReason::GeoRestricted
        );
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {"status": "ERROR", "reason": "This video has been removed by the uploader"}
            })),
            UnavailabilityReason::Deleted
        );
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {"status": "UNPLAYABLE", "reason": "This video is available to this channel's members"}
            })),
            UnavailabilityReason::MembersOnly
        );
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {"status": "ERROR", "reason": "Video unavailable"}
            })),
            UnavailabilityReason::Other
        );
        // The regression: "page" is not "age".
        assert_eq!(
            mapped_unavailability(&json!({
                "playabilityStatus": {"status": "UNPLAYABLE", "reason": "The page needs to be reloaded."}
            })),
            UnavailabilityReason::Other
        );
    }

    #[test]
    fn a_restricted_answer_without_video_details_is_unavailability_not_a_parse_error() {
        // Upstream omits `videoDetails` for bot checks and age gates. Reporting that as a parse
        // failure would blame the parser for an upstream content fact.
        let payload = json!({
            "playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "Sign in to confirm you're not a bot"}
        });
        assert_eq!(
            mapped_unavailability(&payload),
            UnavailabilityReason::LoginRequired
        );
    }

    #[test]
    fn missing_video_identity_is_a_parse_failure() {
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"title": "no id"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let error =
            map_player(&payload, &id, &mut report, provenance()).expect_err("identity is required");
        assert_eq!(error.code(), "provider_parse");
        assert!(error.to_string().contains("videoId"), "{error}");
    }

    #[test]
    fn a_playable_answer_without_microformat_or_captions_still_maps() {
        // Optional sections are optional: their absence is not a defect and must not crash.
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t", "lengthSeconds": "5"},
            "unknownFutureSection": {"nested": [1, 2, 3]}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");
        assert_eq!(document.availability, Availability::Available);
        assert!(document.caption_tracks.is_empty());
        assert!(document.formats.is_empty());
        assert!(document.summary.published_at.is_none());
        assert_eq!(document.summary.duration_seconds, Some(5));
    }

    #[test]
    fn wrong_video_in_response_is_a_parse_error() {
        let payload = json!({
            "videoDetails": {"videoId": "aaaaaaaaaaa", "title": "Other"},
            "playabilityStatus": {"status": "OK"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        assert!(matches!(
            map_player(&payload, &id, &mut report, provenance()),
            Err(ProviderError::Parse { .. })
        ));
    }

    #[test]
    fn caption_tracks_are_mapped() {
        let payload = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "x"},
            "playabilityStatus": {"status": "OK"},
            "captions": {"playerCaptionsTracklistRenderer": {"captionTracks": [
                {"languageCode": "en", "kind": "asr", "baseUrl": "https://example.invalid/timedtext",
                 "isTranslatable": true, "translationLanguages": [{"languageCode": "de"}]},
                {"languageCode": "fr", "baseUrl": "https://example.invalid/timedtext"}
            ]}}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("mapped");
        assert_eq!(document.caption_tracks.len(), 2);
        assert_eq!(document.caption_tracks[0].kind, CaptionKind::Automatic);
        assert!(document.caption_tracks[0].base_url_present);
        assert_eq!(
            document.caption_tracks[0].translation_languages,
            vec!["de".to_owned()]
        );
        assert_eq!(document.caption_tracks[1].kind, CaptionKind::Manual);
    }
}
