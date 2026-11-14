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
                .and_then(|value| text_of(value))
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

fn unavailability_from_status(status: &str, reason: Option<&str>) -> UnavailabilityReason {
    let reason_lower = reason.unwrap_or_default().to_ascii_lowercase();
    if reason_lower.contains("private") {
        return UnavailabilityReason::Private;
    }
    if reason_lower.contains("members") {
        return UnavailabilityReason::MembersOnly;
    }
    if reason_lower.contains("country") || reason_lower.contains("region") {
        return UnavailabilityReason::GeoRestricted;
    }
    if reason_lower.contains("age") {
        return UnavailabilityReason::AgeRestricted;
    }
    if reason_lower.contains("copyright") {
        return UnavailabilityReason::CopyrightBlocked;
    }
    match status {
        "LOGIN_REQUIRED" | "AGE_CHECK_REQUIRED" | "AGE_VERIFICATION_REQUIRED" => {
            UnavailabilityReason::LoginRequired
        }
        "LIVE_STREAM_OFFLINE" => UnavailabilityReason::NotBroadcasting,
        "ERROR" if reason_lower.contains("deleted") || reason_lower.contains("removed") => {
            UnavailabilityReason::Deleted
        }
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

    let details = dto.video_details.ok_or_else(|| ProviderError::Parse {
        message: "player response without videoDetails".into(),
    })?;

    if dto.playability_status.is_none() {
        // Tolerance, not an assertion: a player response is expected to state its playability, so
        // its absence is recorded as a missing field. The document keeps the data it did receive
        // instead of inventing an availability reason, but `parse_warnings` reflects the gap and
        // strict mode turns it into an error.
        report.missing_field("playabilityStatus", "status");
    }

    if let Some(id) = details.video_id.as_deref()
        && id != expected_id.as_str()
    {
        // Documented upstream behaviour: an outdated client can answer with another video
        // (`docs/research/EXTRACTION_METHODS.md` §13).
        return Err(ProviderError::Parse {
            message: format!("wrong result: requested {expected_id}, got {id}"),
        });
    }

    let status = dto
        .playability_status
        .as_ref()
        .and_then(|status| status.status.as_deref())
        .unwrap_or("OK");
    let reason = dto
        .playability_status
        .as_ref()
        .and_then(|status| status.reason.as_deref());

    let availability = match status {
        "OK" => Availability::Available,
        other => {
            let unavailability = unavailability_from_status(other, reason);
            if unavailability != UnavailabilityReason::Other {
                report.warn(format!("playability:{other}"));
            }
            Availability::Unavailable {
                reason: unavailability,
            }
        }
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
    fn player_without_playability_records_a_missing_field() {
        let payload = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "A video", "lengthSeconds": "10"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("document");

        // The status defaults to `OK` (the payload did carry video data) but the gap is never
        // silent: it is a counted warning, so `parse_warnings` and strict mode both see it.
        assert_eq!(document.availability, Availability::Available);
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

    #[test]
    fn playability_status_becomes_availability_with_reason() {
        let payload = json!({
            "videoDetails": {"videoId": "dQw4w9WgXcQ", "title": "Private video"},
            "playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "This video is private"}
        });
        let id = VideoId::parse("dQw4w9WgXcQ").expect("id");
        let mut report = ParseReport::new();
        let document = map_player(&payload, &id, &mut report, provenance()).expect("mapped");
        assert_eq!(
            document.availability,
            Availability::Unavailable {
                reason: UnavailabilityReason::Private
            }
        );
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
