//! Mapping: InnerTube JSON → domain models.
//!
//! Search results are mapped from `serde_json::Value` with explicit helpers rather than a full DTO
//! mirror: renderer shapes vary per surface (search / grid / shelf) and a mirror would break on every
//! permutation (ADR-002). The player response has a stable top-level shape, so it is typed.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use vydra_common::text::{is_abbreviated_count, parse_clock_duration, parse_view_count};
use vydra_common::{ChannelId, PlaylistId, VideoId};
use vydra_domain::{
    Availability, BroadcastInfo, CaptionKind, CaptionTrack, Chapter, Count, DeliveryKind,
    FieldConflict, FieldSource, LiveState, MediaFormatInfo, MetadataEvidence, PlaylistSummary,
    Provenance, Publication, SearchItem, SearchResults, Storyboard, StoryboardLevel, Thumbnail,
    VideoDocument, VideoSummary,
};
use vydra_errors::{ProviderError, UnavailabilityReason};

use crate::parse::{ParseReport, Renderer, find_continuation, number_of, text_of, thumbnails_of};

pub(crate) fn first_text(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(text_of) {
            return Some(text);
        }
    }
    None
}

pub(crate) fn count_from_text(raw: &str) -> Option<Count> {
    let value = parse_view_count(raw)?;
    if is_abbreviated_count(raw) {
        Some(Count::approximate(value, raw))
    } else {
        Some(Count::exact(value))
    }
}

/// Exact numeric fields win over localized text (ADR-016).
pub(crate) fn count_from_fields(
    value: &Value,
    numeric_key: &str,
    text_keys: &[&str],
) -> Option<Count> {
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
pub(crate) fn channel_id_of(renderer: &Value, keys: &[&str]) -> Option<ChannelId> {
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
    microformat: Option<MicroformatDto>,
    captions: Option<Value>,
    streaming_data: Option<StreamingDataDto>,
    storyboards: Option<StoryboardsDto>,
}

/// `playabilityStatus`. Textual members are held as [`Value`] and read through `text_of`, so a single
/// unexpected member type cannot reject the whole response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayabilityStatusDto {
    status: Option<Value>,
    reason: Option<Value>,
    /// Upstream's own explanation, e.g. `[{"text": "This video is private"}]`. Used to *classify*
    /// the restriction and never copied into a document.
    messages: Option<Value>,
    /// `errorScreen.playerErrorMessageRenderer`: the rendered explanation for `LOGIN_REQUIRED` and
    /// friends, which often carries the wording the `reason` field omits.
    error_screen: Option<Value>,
}

/// `videoDetails`. Every member is optional and every member is read through a helper, because a
/// response with one wrong-typed field must still yield the fields that are readable.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetailsDto {
    video_id: Option<Value>,
    title: Option<Value>,
    length_seconds: Option<Value>,
    view_count: Option<Value>,
    author: Option<Value>,
    channel_id: Option<Value>,
    short_description: Option<Value>,
    keywords: Option<Value>,
    is_live_content: Option<Value>,
    is_live_now: Option<Value>,
    is_upcoming: Option<Value>,
    /// `{"thumbnails": [...]}` — the same shape as the microformat's `thumbnail`, used as a fallback
    /// when the optional `microformat` object is absent (Phase 5A live capture: 5 sizes here, 1
    /// there).
    thumbnail: Option<Value>,
}

/// `microformat`. The renderer is nested under `playerMicroformatRenderer` in every capture; the
/// `microformat` alias exists because an older shape wrapped it under its own name.
#[derive(Debug, Deserialize)]
struct MicroformatDto {
    #[serde(rename = "playerMicroformatRenderer", alias = "microformat")]
    renderer: Option<MicroformatRendererDto>,
}

/// `microformat.playerMicroformatRenderer` (Phase 5B).
///
/// Values are held as [`Value`] where upstream has changed type between recordings (numbers arrive as
/// strings in every capture, but not everywhere in the ecosystem) so that a type change degrades one
/// field instead of failing the document.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MicroformatRendererDto {
    publish_date: Option<Value>,
    upload_date: Option<Value>,
    category: Option<Value>,
    title: Option<Value>,
    description: Option<Value>,
    owner_channel_name: Option<Value>,
    external_channel_id: Option<Value>,
    length_seconds: Option<Value>,
    view_count: Option<Value>,
    like_count: Option<Value>,
    is_unlisted: Option<Value>,
    is_family_safe: Option<Value>,
    thumbnail: Option<Value>,
    live_broadcast_details: Option<LiveBroadcastDetailsDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LiveBroadcastDetailsDto {
    is_live_now: Option<Value>,
    start_timestamp: Option<Value>,
    end_timestamp: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoryboardsDto {
    player_storyboard_spec_renderer: Option<StoryboardSpecDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoryboardSpecDto {
    /// `url_template|level|level…`; the template is dropped, the levels are parsed.
    spec: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingDataDto {
    formats: Option<Vec<Value>>,
    adaptive_formats: Option<Vec<Value>>,
}

/// Read a JSON boolean that may arrive as `true`, `"true"` or `1`.
fn flag(raw: &Option<Value>) -> Option<bool> {
    match raw.as_ref()? {
        Value::Bool(value) => Some(*value),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" => Some(true),
            "false" | "no" => Some(false),
            _ => None,
        },
        Value::Number(number) => number.as_u64().map(|value| value != 0),
        _ => None,
    }
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
        // The bound keeps a huge description or promo blob from being copied into classification
        // text; anything past it is ignored rather than truncated mid-word into a false match.
        Value::String(text) if out.len() < 512 => {
            out.push(' ');
            out.push_str(text);
        }
        _ => {}
    }
}

/// Lowercased classification text: the `reason` field, then whatever `messages` and `errorScreen`
/// say. `LOGIN_REQUIRED` explains the age gate there rather than in `reason`.
fn playability_detail(status: &PlayabilityStatusDto) -> String {
    let mut text = String::new();
    if let Some(reason) = status.reason.as_ref().and_then(text_of) {
        text.push_str(&reason);
    }
    if let Some(messages) = &status.messages {
        collect_strings(messages, &mut text);
    }
    if let Some(screen) = &status.error_screen {
        collect_strings(screen, &mut text);
    }
    text.to_ascii_lowercase()
}

fn classify_playability(status: &PlayabilityStatusDto) -> Playability {
    let Some(raw) = status.status.as_ref().and_then(text_of) else {
        return Playability::Missing;
    };
    if raw == "OK" {
        return Playability::Ok;
    }
    let detail = playability_detail(status);
    let normalized = if detail.trim().is_empty() {
        None
    } else {
        Some(detail.as_str())
    };
    match unavailability_from_status(&raw, normalized) {
        UnavailabilityReason::Other => Playability::Unrecognised,
        reason => Playability::Restricted(reason),
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
    let raw = status.status.as_ref().and_then(text_of);
    let raw = raw.as_deref().unwrap_or("absent");
    let detail: Option<String> = status.reason.as_ref().and_then(text_of).map(|reason| {
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
///
/// Precedence between the two metadata sections is fixed and documented in
/// `docs/research/VIDEO_METADATA_CONTRACT.md` §3: `videoDetails` is primary for everything it carries,
/// `microformat` fills the gaps (`publishDate`, `likeCount`, `category`, `isUnlisted`) and is a fallback
/// for the rest. Where both carry a value and they disagree, the primary wins **and the disagreement is
/// recorded** in `document.evidence.conflicts` — the live capture disagrees about duration (213 vs 214 s),
/// which is exactly the kind of thing a silently-chosen number would hide forever.
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
    let raw_status = playability
        .and_then(|status| status.status.as_ref())
        .and_then(text_of);
    let verdict = match playability {
        Some(status) => classify_playability(status),
        None => Playability::Missing,
    };
    if verdict == Playability::Missing {
        report.missing_field("playabilityStatus", "status");
    } else if verdict != Playability::Ok
        && let Some(status) = raw_status.as_deref()
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
    let upstream_id =
        details
            .video_id
            .as_ref()
            .and_then(text_of)
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

    let mut evidence = MetadataEvidence::default();
    evidence.section(FieldSource::VideoDetails);

    let micro = dto
        .microformat
        .as_ref()
        .and_then(|microformat| microformat.renderer.as_ref());
    if micro.is_some() {
        evidence.section(FieldSource::Microformat);
    }
    if dto.streaming_data.is_some() {
        evidence.section(FieldSource::StreamingData);
    }
    if dto.captions.is_some() {
        evidence.section(FieldSource::Captions);
    }

    // --- text fields -------------------------------------------------------------
    let title = match details.title.as_ref().and_then(text_of) {
        Some(title) => Some(title),
        None => micro
            .and_then(|micro| micro.title.as_ref())
            .and_then(text_of)
            .inspect(|_| evidence.fallback("title", FieldSource::Microformat)),
    };
    let title = title.unwrap_or_else(|| {
        report.missing_field("videoDetails", "title");
        String::new()
    });

    let description = match details.short_description.as_ref().and_then(text_of) {
        Some(text) => Some(text),
        None => micro
            .and_then(|micro| micro.description.as_ref())
            .and_then(text_of)
            .inspect(|_| evidence.fallback("description", FieldSource::Microformat)),
    };

    let channel_id = match details
        .channel_id
        .as_ref()
        .and_then(text_of)
        .and_then(|raw| ChannelId::parse(&raw).ok())
    {
        Some(id) => Some(id),
        None => micro
            .and_then(|micro| micro.external_channel_id.as_ref())
            .and_then(text_of)
            .and_then(|raw| ChannelId::parse(&raw).ok())
            .inspect(|_| evidence.fallback("channel_id", FieldSource::Microformat)),
    };

    let channel_title = match details.author.as_ref().and_then(text_of) {
        Some(author) => Some(author),
        None => micro
            .and_then(|micro| micro.owner_channel_name.as_ref())
            .and_then(text_of)
            .inspect(|_| evidence.fallback("channel_title", FieldSource::Microformat)),
    };

    // --- numbers -----------------------------------------------------------------
    let duration_seconds = resolve_u32(
        "duration_seconds",
        details.length_seconds.as_ref(),
        FieldSource::VideoDetails,
        "lengthSeconds",
        micro.and_then(|micro| micro.length_seconds.as_ref()),
        report,
        &mut evidence,
    );

    let view_count = resolve_count(
        "view_count",
        details.view_count.as_ref(),
        FieldSource::VideoDetails,
        "viewCount",
        micro.and_then(|micro| micro.view_count.as_ref()),
        report,
        &mut evidence,
    );

    // `likeCount` has exactly one source: the microformat. It is a plain integer string there, so it is
    // an exact count; when the key is absent the field stays `None` rather than becoming 0.
    let like_count = match micro.and_then(|micro| micro.like_count.as_ref()) {
        Some(raw) => required_number(raw, "microformat", "likeCount", report).map(Count::exact),
        None => None,
    };

    // --- publication -------------------------------------------------------------
    let publication = publication_from(micro, report);
    let published_at = publication
        .as_ref()
        .map(|publication| publication.published_at);

    // --- broadcast state ---------------------------------------------------------
    let broadcast = broadcast_from(micro);
    let live_state = player_live_state(&details, broadcast.as_ref(), &mut evidence);

    let availability = match verdict {
        Playability::Ok => match live_state {
            // An `OK` verdict on a live broadcast is not the same fact as an `OK` verdict on a
            // published upload: the metadata above it changes while it airs, which is a cache concern.
            Some(LiveState::Live) => Availability::Live,
            Some(LiveState::Upcoming) => Availability::Upcoming,
            _ => Availability::Available,
        },
        Playability::Restricted(reason) => Availability::Unavailable { reason },
        Playability::Unrecognised => Availability::Unavailable {
            reason: UnavailabilityReason::Other,
        },
        // The document keeps the data it did receive, but never claims availability: `Unknown` is not
        // a synonym for `Available`, is never cached and is never playable.
        Playability::Missing => Availability::Unknown,
    };

    let category = micro
        .and_then(|micro| micro.category.as_ref())
        .and_then(text_of);
    let unlisted = micro.and_then(|micro| flag(&micro.is_unlisted));
    let family_safe = micro.and_then(|micro| flag(&micro.is_family_safe));

    // `microformat` is optional but `videoDetails.thumbnail` is not: without this fallback a real
    // document would map to zero thumbnails purely because one optional section was missing.
    let mut thumbnails: Vec<Thumbnail> = micro
        .and_then(|micro| micro.thumbnail.as_ref())
        .map(thumbnails_of)
        .unwrap_or_default();
    if thumbnails.is_empty()
        && let Some(fallback) = details.thumbnail.as_ref().map(thumbnails_of)
        && !fallback.is_empty()
    {
        thumbnails = fallback;
        evidence.fallback("thumbnails", FieldSource::VideoDetails);
    }

    let keywords = details
        .keywords
        .as_ref()
        .and_then(Value::as_array)
        .map(|keywords| {
            keywords
                .iter()
                .filter_map(|keyword| keyword.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let chapters = description
        .as_deref()
        .map(parse_chapters)
        .unwrap_or_default();
    if !chapters.is_empty() {
        evidence.section(FieldSource::Description);
    }

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

    let storyboards = storyboards_from(dto.storyboards.as_ref(), report, &mut evidence);

    Ok(VideoDocument {
        summary: VideoSummary {
            id: expected_id.clone(),
            title,
            channel_id,
            channel_title,
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
        keywords,
        like_count,
        comment_count: None,
        chapters,
        caption_tracks,
        formats,
        storyboards,
        publication,
        broadcast,
        unlisted,
        family_safe,
        evidence,
        provenance,
    })
}

/// Read a numeric field that must be a plain integer, reporting when it is present but unreadable.
///
/// The distinction matters: an absent key is upstream saying nothing (a live broadcast has no
/// duration), while a present-but-unreadable key is a parse loss and is counted as one.
fn required_number(
    raw: &Value,
    section: &str,
    field: &str,
    report: &mut ParseReport,
) -> Option<u64> {
    match number_of(raw) {
        Some(value) => Some(value),
        None => {
            report.missing_field(section, field);
            None
        }
    }
}

/// Optional-value wrapper around [`required_number`]: absent stays silent.
fn optional_number(
    raw: Option<&Value>,
    section: &str,
    field: &str,
    report: &mut ParseReport,
) -> Option<u64> {
    raw.and_then(|raw| required_number(raw, section, field, report))
}

/// Primary/fallback resolution for an integer field, recording the fallback or the conflict.
fn resolve_u32(
    field: &str,
    primary: Option<&Value>,
    primary_source: FieldSource,
    primary_name: &str,
    secondary: Option<&Value>,
    report: &mut ParseReport,
    evidence: &mut MetadataEvidence,
) -> Option<u32> {
    let (value, source) = resolve_number(
        field,
        primary,
        primary_source,
        primary_name,
        secondary,
        report,
        evidence,
    )?;
    match u32::try_from(value) {
        Ok(value) => Some(value),
        Err(_) => {
            // Out of range for the domain type: a loss, not a value to clamp.
            report.missing_field(source.as_str(), field);
            None
        }
    }
}

fn resolve_count(
    field: &str,
    primary: Option<&Value>,
    primary_source: FieldSource,
    primary_name: &str,
    secondary: Option<&Value>,
    report: &mut ParseReport,
    evidence: &mut MetadataEvidence,
) -> Option<Count> {
    resolve_number(
        field,
        primary,
        primary_source,
        primary_name,
        secondary,
        report,
        evidence,
    )
    .map(|(value, _)| Count::exact(value))
}

/// Resolve a field carried by two sections, returning the value and the section it came from.
fn resolve_number(
    field: &str,
    primary: Option<&Value>,
    primary_source: FieldSource,
    primary_name: &str,
    secondary: Option<&Value>,
    report: &mut ParseReport,
    evidence: &mut MetadataEvidence,
) -> Option<(u64, FieldSource)> {
    let primary_value = optional_number(primary, primary_source.as_str(), primary_name, report);
    let secondary_value = optional_number(secondary, "microformat", field, report);
    match (primary_value, secondary_value) {
        (Some(kept), Some(other)) => {
            if kept != other {
                evidence.conflict(FieldConflict {
                    field: field.to_owned(),
                    kept: primary_source,
                    kept_value: kept.to_string(),
                    rejected: FieldSource::Microformat,
                    rejected_value: other.to_string(),
                });
                report.warn(format!("conflict:{field}:{kept}!={other}"));
            }
            Some((kept, primary_source))
        }
        (Some(kept), None) => Some((kept, primary_source)),
        (None, Some(value)) => {
            evidence.fallback(field, FieldSource::Microformat);
            Some((value, FieldSource::Microformat))
        }
        (None, None) => None,
    }
}

/// `microformat.publishDate` with the upstream text and offset preserved (`crate::Publication`).
fn publication_from(
    micro: Option<&MicroformatRendererDto>,
    report: &mut ParseReport,
) -> Option<Publication> {
    let raw = micro?.publish_date.as_ref()?.as_str()?.trim().to_owned();
    let parsed = match OffsetDateTime::parse(&raw, &Rfc3339) {
        Ok(parsed) => parsed,
        Err(_) => {
            report.missing_field("microformat", "publishDate");
            return None;
        }
    };

    // ADR-016: timestamps are RFC 3339 **UTC** on the wire. The live capture publishes
    // `2009-10-24T23:57:33-07:00`, so the value is normalised here and the original kept beside it.
    let mut publication = Publication {
        published_at: parsed.to_offset(UtcOffset::UTC),
        upstream_text: raw,
        upstream_offset_seconds: parsed.offset().whole_seconds(),
        uploaded_at: None,
        uploaded_text: None,
    };

    if let Some(upload_raw) = micro
        .and_then(|micro| micro.upload_date.as_ref())
        .and_then(Value::as_str)
        .map(|text| text.trim().to_owned())
        && let Ok(uploaded) = OffsetDateTime::parse(&upload_raw, &Rfc3339)
    {
        publication.uploaded_at = Some(uploaded.to_offset(UtcOffset::UTC));
        publication.uploaded_text = Some(upload_raw);
    }

    Some(publication)
}

/// `microformat.liveBroadcastDetails`, when upstream states a broadcast window.
fn broadcast_from(micro: Option<&MicroformatRendererDto>) -> Option<BroadcastInfo> {
    let details = micro?.live_broadcast_details.as_ref()?;
    let start_at = details
        .start_timestamp
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|raw| OffsetDateTime::parse(raw, &Rfc3339).ok())
        .map(|start| start.to_offset(UtcOffset::UTC));
    let end_at = details
        .end_timestamp
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|raw| OffsetDateTime::parse(raw, &Rfc3339).ok())
        .map(|end| end.to_offset(UtcOffset::UTC));
    let live_now = flag(&details.is_live_now);

    if live_now.is_none() && start_at.is_none() && end_at.is_none() {
        return None;
    }
    Some(BroadcastInfo {
        live_now,
        start_at,
        end_at,
    })
}

/// Broadcast state: `videoDetails` flags are primary, `liveBroadcastDetails` is the fallback.
///
/// Named `player_live_state` because `live_state_of` already means the search renderer's overlay check.
fn player_live_state(
    details: &VideoDetailsDto,
    broadcast: Option<&BroadcastInfo>,
    evidence: &mut MetadataEvidence,
) -> Option<LiveState> {
    if let Some(state) = details_flag_state(details) {
        return Some(state);
    }
    let broadcast = broadcast?;
    let state = match (broadcast.live_now, broadcast.end_at, broadcast.start_at) {
        (Some(true), _, _) => Some(LiveState::Live),
        (_, Some(_), _) => Some(LiveState::Ended),
        (Some(false), None, Some(start)) if start > OffsetDateTime::now_utc() => {
            Some(LiveState::Upcoming)
        }
        (Some(false), None, Some(_)) => Some(LiveState::NotLive),
        _ => None,
    };
    if state.is_some() {
        evidence.fallback("live_state", FieldSource::Microformat);
    }
    state
}

/// `videoDetails` live flags, in their own precedence (`live` beats `upcoming` beats `not live`).
fn details_flag_state(details: &VideoDetailsDto) -> Option<LiveState> {
    if flag(&details.is_live_now) == Some(true) {
        Some(LiveState::Live)
    } else if flag(&details.is_upcoming) == Some(true) {
        Some(LiveState::Upcoming)
    } else if flag(&details.is_live_content) == Some(true) {
        Some(LiveState::NotLive)
    } else {
        None
    }
}

/// Storyboard geometry from `storyboards.playerStoryboardSpecRenderer.spec`.
///
/// The spec is `url_template|width#height#frames#cols#rows#interval_ms#name#sigh|…`. Only the numbers
/// are modelled; the template is a signed image URL and is never stored (ADR-006). A spec this build
/// cannot read produces **no** storyboards and one counted warning — never a guess.
fn storyboards_from(
    section: Option<&StoryboardsDto>,
    report: &mut ParseReport,
    evidence: &mut MetadataEvidence,
) -> Option<Storyboard> {
    let spec = section
        .and_then(|section| section.player_storyboard_spec_renderer.as_ref())
        .and_then(|renderer| renderer.spec.as_ref())
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|spec| !spec.is_empty())?;

    let mut parts = spec.split('|');
    let template = parts.next().unwrap_or_default();
    let mut levels = Vec::new();
    for level in parts {
        let fields: Vec<&str> = level.split('#').collect();
        if fields.len() < 6 {
            report.warn("storyboards_unparsed");
            return None;
        }
        let parsed = fields.get(0..6).and_then(|fields| {
            let mut numbers = [0u32; 6];
            for (slot, raw) in numbers.iter_mut().zip(fields) {
                *slot = raw.trim().parse::<u32>().ok()?;
            }
            Some(numbers)
        });
        let Some([width, height, frame_count, columns, rows, interval_ms]) = parsed else {
            report.warn("storyboards_unparsed");
            return None;
        };
        if width == 0 || height == 0 || columns == 0 || rows == 0 {
            report.warn("storyboards_unparsed");
            return None;
        }
        let name = fields
            .get(6)
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let sigh_present = fields.get(7).is_some_and(|sigh| !sigh.trim().is_empty());
        levels.push(StoryboardLevel {
            width,
            height,
            frame_count,
            columns,
            rows,
            interval_ms,
            name,
            sigh_present,
        });
    }

    if levels.is_empty() {
        report.warn("storyboards_unparsed");
        return None;
    }
    evidence.section(FieldSource::Storyboards);
    Some(Storyboard {
        levels,
        url_template_present: template.starts_with("http"),
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
    fn thumbnails_fall_back_to_video_details_when_microformat_is_absent() {
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "t",
                "thumbnail": {"thumbnails": [
                    {"url": "//i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg", "width": 480, "height": 360},
                    {"url": "//i.ytimg.com/vi/dQw4w9WgXcQ/maxresdefault.jpg", "width": 1280, "height": 720}
                ]}
            }
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");
        assert_eq!(document.summary.thumbnails.len(), 2);
        assert_eq!(document.summary.thumbnails[1].width, Some(1280));
        assert!(document.summary.thumbnails[0].url.starts_with("https://"));
    }

    // ---------------------------------------------------------------------------------------
    // Phase 5B: microformat, publication, broadcast, storyboards, evidence
    // ---------------------------------------------------------------------------------------

    fn player_with(microformat: Value, details: Value) -> Value {
        json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": details,
            "microformat": {"playerMicroformatRenderer": microformat}
        })
    }

    fn map(payload: &Value) -> (VideoDocument, ParseReport) {
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(payload, &id, &mut report, provenance()).expect("document");
        (document, report)
    }

    #[test]
    fn like_count_is_mapped_from_the_only_section_that_carries_it() {
        let (document, _) = map(&player_with(
            json!({"likeCount": "19503628"}),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        ));
        let like = document.like_count.as_ref().expect("like count");
        assert_eq!(like.value, 19_503_628);
        assert_eq!(like.precision, Precision::Exact);

        // Absent is not zero.
        let (document, _) = map(&player_with(
            json!({}),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        ));
        assert!(document.like_count.is_none());
    }

    #[test]
    fn publication_keeps_the_offset_while_the_document_stays_utc() {
        let (document, _) = map(&player_with(
            json!({
                "publishDate": "2009-10-24T23:57:33-07:00",
                "uploadDate": "2009-10-24T23:57:33-07:00"
            }),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        ));
        let publication = document.publication.as_ref().expect("publication");
        assert_eq!(publication.upstream_text, "2009-10-24T23:57:33-07:00");
        assert_eq!(publication.upstream_offset_seconds, -25_200);
        assert_eq!(publication.published_at.offset(), UtcOffset::UTC);
        assert_eq!(publication.published_at.hour(), 6);
        assert_eq!(publication.published_at.day(), 25);
        assert_eq!(
            publication.uploaded_at.map(|uploaded| uploaded.offset()),
            Some(UtcOffset::UTC)
        );
        // The summary keeps carrying the same instant, so existing clients are unaffected.
        assert_eq!(
            document.summary.published_at,
            Some(publication.published_at)
        );
    }

    #[test]
    fn unparsable_publish_date_is_counted_not_invented() {
        let (document, report) = map(&player_with(
            json!({"publishDate": "yesterday"}),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        ));
        assert!(document.summary.published_at.is_none());
        assert!(document.publication.is_none());
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message == "missing_field:microformat.publishDate"),
            "{:?}",
            report.messages()
        );
    }

    #[test]
    fn disagreeing_sections_keep_the_primary_value_and_record_the_conflict() {
        // The live capture really does disagree: videoDetails says 213 s, microformat 214 s.
        let (document, report) = map(&player_with(
            json!({"lengthSeconds": "214", "viewCount": "1836979877"}),
            json!({
                "videoId": "dQw4w9WgXcQ",
                "title": "t",
                "lengthSeconds": "213",
                "viewCount": "1836979877"
            }),
        ));
        assert_eq!(document.summary.duration_seconds, Some(213));
        let conflict = document
            .evidence
            .conflicts
            .iter()
            .find(|conflict| conflict.field == "duration_seconds")
            .expect("duration conflict");
        assert_eq!(conflict.kept, FieldSource::VideoDetails);
        assert_eq!(conflict.kept_value, "213");
        assert_eq!(conflict.rejected, FieldSource::Microformat);
        assert_eq!(conflict.rejected_value, "214");
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message == "conflict:duration_seconds:213!=214"),
            "{:?}",
            report.messages()
        );
        // Agreement is not a conflict.
        assert!(
            !document
                .evidence
                .conflicts
                .iter()
                .any(|conflict| conflict.field == "view_count")
        );
    }

    #[test]
    fn microformat_fills_gaps_and_the_fallback_is_recorded() {
        // `videoDetails` carries only identity: everything else must come from the microformat,
        // and each of those fields is a recorded fallback rather than an invisible substitution.
        let (document, _) = map(&player_with(
            json!({
                "title": {"simpleText": "Microformat title"},
                "description": {"simpleText": "Microformat description"},
                "externalChannelId": "UCuAXFkgsw1L7xaCfnd5JJOw",
                "ownerChannelName": "Rick Astley",
                "lengthSeconds": "213",
                "viewCount": "1836979877",
                "category": "Music",
                "isUnlisted": false,
                "isFamilySafe": true
            }),
            json!({"videoId": "dQw4w9WgXcQ"}),
        ));

        assert_eq!(document.summary.title, "Microformat title");
        assert_eq!(
            document.description.as_deref(),
            Some("Microformat description")
        );
        assert_eq!(
            document.summary.channel_id.as_ref().map(|id| id.as_str()),
            Some("UCuAXFkgsw1L7xaCfnd5JJOw")
        );
        assert_eq!(
            document.summary.channel_title.as_deref(),
            Some("Rick Astley")
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
        assert_eq!(document.category.as_deref(), Some("Music"));
        assert_eq!(document.unlisted, Some(false));
        assert_eq!(document.family_safe, Some(true));

        let fallbacks: Vec<&str> = document
            .evidence
            .fallbacks
            .iter()
            .map(|fallback| fallback.field.as_str())
            .collect();
        for field in [
            "title",
            "description",
            "channel_id",
            "channel_title",
            "duration_seconds",
            "view_count",
        ] {
            assert!(
                fallbacks.contains(&field),
                "{field} missing from {fallbacks:?}"
            );
        }
        assert!(
            document.evidence.has_section(FieldSource::Microformat)
                && document.evidence.has_section(FieldSource::VideoDetails)
        );
    }

    #[test]
    fn a_present_but_unreadable_number_is_a_counted_loss() {
        let (document, report) = map(&player_with(
            json!({}),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t", "lengthSeconds": "two minutes"}),
        ));
        assert!(
            document.summary.duration_seconds.is_none(),
            "never 0, never a guess"
        );
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message == "missing_field:videoDetails.lengthSeconds"),
            "{:?}",
            report.messages()
        );
        // An absent field is upstream saying nothing and is not counted.
        let (_, report) = map(&player_with(
            json!({}),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        ));
        assert!(
            !report
                .messages()
                .iter()
                .any(|message| message.contains("lengthSeconds"))
        );
    }

    #[test]
    fn a_live_verdict_becomes_live_availability_not_available() {
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t", "isLiveNow": true}
        });
        let (document, _) = map(&payload);
        assert_eq!(document.availability, Availability::Live);
        assert_eq!(document.summary.live_state, Some(LiveState::Live));
        assert!(
            document.availability.is_available(),
            "live content is playable"
        );

        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t", "isUpcoming": true}
        });
        let (document, _) = map(&payload);
        assert_eq!(document.availability, Availability::Upcoming);
    }

    #[test]
    fn broadcast_details_fill_in_when_video_details_stays_silent() {
        let payload = player_with(
            json!({
                "liveBroadcastDetails": {
                    "isLiveNow": true,
                    "startTimestamp": "2026-11-14T18:00:00Z",
                    "endTimestamp": "2026-11-14T19:30:00Z"
                }
            }),
            json!({"videoId": "dQw4w9WgXcQ", "title": "t"}),
        );
        let (document, _) = map(&payload);
        assert_eq!(document.availability, Availability::Live);
        assert_eq!(document.summary.live_state, Some(LiveState::Live));
        let broadcast = document.broadcast.as_ref().expect("broadcast window");
        assert_eq!(broadcast.live_now, Some(true));
        assert!(broadcast.start_at.is_some() && broadcast.end_at.is_some());
        assert!(
            document
                .evidence
                .fallbacks
                .iter()
                .any(|fallback| fallback.field == "live_state"
                    && fallback.source == FieldSource::Microformat)
        );
    }

    #[test]
    fn storyboard_spec_is_reduced_to_geometry_and_never_to_a_url() {
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t"},
            "storyboards": {"playerStoryboardSpecRenderer": {"spec": "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L$L/$N.jpg?sqp=SECRET&sigh=SIGNED|48#27#100#10#10#0#default#SIG|160#90#50#5#5#1000#M$M#SIG2", "recommendedLevel": 1}}
        });
        let (document, _) = map(&payload);
        let storyboard = document.storyboards.as_ref().expect("storyboard");
        assert!(storyboard.url_template_present);
        assert_eq!(storyboard.levels.len(), 2);
        assert_eq!(storyboard.levels[0].width, 48);
        assert_eq!(storyboard.levels[0].frame_count, 100);
        assert_eq!(storyboard.levels[0].columns, 10);
        assert_eq!(storyboard.levels[0].interval_ms, 0);
        assert_eq!(storyboard.levels[0].name.as_deref(), Some("default"));
        assert!(storyboard.levels[0].sigh_present);
        assert_eq!(storyboard.levels[1].width, 160);
        assert_eq!(storyboard.levels[1].name.as_deref(), Some("M$M"));

        // The signature and the template are not in the document at all.
        let json = serde_json::to_string(&document).expect("serialize");
        assert!(!json.contains("SECRET"), "the template query string leaked");
        assert!(!json.contains("SIGNED"));
        assert!(!json.contains("i.ytimg.com/sb"));
    }

    #[test]
    fn an_unreadable_storyboard_spec_yields_nothing_and_says_so() {
        let payload = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "t"},
            "storyboards": {"playerStoryboardSpecRenderer": {"spec": "https://example.invalid/sb|48#27"}}
        });
        let (document, report) = map(&payload);
        assert!(
            document.storyboards.is_none(),
            "half a spec is not a storyboard"
        );
        assert!(
            report
                .messages()
                .iter()
                .any(|message| message == "storyboards_unparsed"),
            "{:?}",
            report.messages()
        );
    }

    #[test]
    fn a_wrong_typed_member_costs_one_field_not_the_document() {
        // `lengthSeconds` as an object, `keywords` as a string, `isLiveNow` as text: the response is
        // still usable, and none of the three can crash the mapping.
        let payload = json!({
            "playabilityStatus": {"status": "OK", "reason": 5},
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "t",
                "lengthSeconds": {"unexpected": true},
                "keywords": "not-a-list",
                "isLiveNow": "true",
                "viewCount": "12"
            }
        });
        let (document, _) = map(&payload);
        assert_eq!(document.summary.title, "t");
        assert!(document.summary.duration_seconds.is_none());
        assert!(document.keywords.is_empty());
        assert_eq!(document.summary.live_state, Some(LiveState::Live));
        assert_eq!(
            document
                .summary
                .view_count
                .as_ref()
                .map(|count| count.value),
            Some(12)
        );
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
