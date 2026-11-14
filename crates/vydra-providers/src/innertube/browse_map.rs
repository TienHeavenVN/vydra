//! Mapping: InnerTube **browse** JSON → channel and playlist documents (Phase 6).
//!
//! Split from `map.rs` because browse pages have a different failure model from search pages:
//!
//! * a search page is a *list*, so one broken item is a warning;
//! * a channel or playlist page is a *document*, so a missing header means the response is not the page
//!   that was asked for, and the call fails instead of returning a document with invented fields.
//!
//! Two structural rules are inherited from the search mapper and one is new:
//!
//! 1. Renderers are discovered by key, never by a fixed path (`twoColumnBrowseResultsRenderer` is not
//!    stable across surfaces).
//! 2. A recognized-but-unmapped content shape is counted (`unmapped_result`), not guessed at.
//! 3. **Pagination comes only from `continuationItemRenderer.continuationEndpoint.continuationCommand.token`.**
//!    Channel and playlist pages also carry *filter chips* (`chipCloudChipRenderer`) and *sort menus*
//!    (`sortFilterSubMenuRenderer`) whose entries are `continuationCommand` tokens that re-sort the same
//!    list. `/tmp` research note: this is exactly the confusion that `parse::find_page_cursor` exists to
//!    prevent — see `docs/research/CHANNEL_CONTRACT.md` §5.

use std::collections::HashSet;

use serde_json::Value;
use vydra_common::text::parse_clock_duration;
use vydra_common::{ChannelId, ChannelRef, PlaylistId, VideoId};
use vydra_domain::{
    Availability, ChannelDocument, ChannelSummary, ChannelTab, ChannelVerification,
    ChannelVideoList, Count, MetadataEvidence, PlaylistDocument, PlaylistEntrySource, PlaylistItem,
    PlaylistSummary, PlaylistVideoList, Provenance, SearchItem, Thumbnail,
};
use vydra_errors::{ProviderError, UnavailabilityReason};

use crate::parse::{
    ParseReport, Renderer, collect_renderers, find_continuation, find_page_cursor, text_of,
    thumbnails_of,
};

use super::map::{channel_id_of, count_from_text, first_text};

/// Keys whose tab titles are stable enough to be given a slug.
///
/// The slug is what the API and the client use to *select* a tab; it is derived from the title the page
/// rendered, so an unrecognized title simply has no slug instead of a guessed one.
const TAB_SLUGS: &[(&str, &str)] = &[
    ("home", "home"),
    ("videos", "videos"),
    ("shorts", "shorts"),
    ("live", "live"),
    ("playlists", "playlists"),
    ("podcasts", "podcasts"),
    ("releases", "releases"),
    ("courses", "courses"),
    ("community", "community"),
    ("about", "about"),
    ("store", "store"),
    ("channels", "channels"),
];

fn tab_slug(title: &str) -> Option<String> {
    let lowered = title.trim().to_lowercase();
    TAB_SLUGS
        .iter()
        .find(|(needle, _)| *needle == lowered)
        .map(|(_, slug)| (*slug).to_owned())
}

fn thumbnails_from_sources(sources: &Value) -> Vec<Thumbnail> {
    let Some(items) = sources.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<Thumbnail> = items
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?;
            Some(Thumbnail {
                url: vydra_common::text::normalize_thumbnail_url(url)
                    .unwrap_or_else(|| url.to_owned()),
                width: item
                    .get("width")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                height: item
                    .get("height")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            })
        })
        .collect();
    out.sort_by_key(|thumb| thumb.width.unwrap_or(0));
    out
}

fn largest(thumbnails: Vec<Thumbnail>) -> Option<Thumbnail> {
    thumbnails.into_iter().max_by_key(|thumb| thumb.width.unwrap_or(0))
}

/// `metadata.channelMetadataRenderer`.
fn channel_metadata(payload: &Value) -> Option<&Value> {
    payload.get("metadata")?.get("channelMetadataRenderer")
}

/// The channel header, in either of the two shapes upstream serves.
fn channel_header(payload: &Value) -> Option<&Value> {
    let header = payload.get("header")?;
    header
        .get("pageHeaderRenderer")
        .or_else(|| header.get("c4TabbedHeaderRenderer"))
}

/// `header.pageHeaderRenderer.content.pageHeaderViewModel` (current shape only).
fn page_header_view_model(header: &Value) -> Option<&Value> {
    header
        .get("content")?
        .get("pageHeaderViewModel")
        .filter(|value| value.is_object())
}

fn avatar_of(header: &Value, metadata: Option<&Value>) -> Vec<Thumbnail> {
    if let Some(view) = page_header_view_model(header) {
        let image = view.get("image");
        if let Some(sources) = image
            .and_then(|image| image.get("decoratedAvatarViewModel"))
            .and_then(|avatar| avatar.get("avatar"))
            .and_then(|avatar| avatar.get("avatarViewModel"))
            .and_then(|avatar| avatar.get("image"))
            .and_then(|image| image.get("sources"))
        {
            let thumbnails = thumbnails_from_sources(sources);
            if !thumbnails.is_empty() {
                return thumbnails;
            }
        }
        if let Some(sources) = image
            .and_then(|image| image.get("avatarViewModel"))
            .and_then(|avatar| avatar.get("image"))
            .and_then(|image| image.get("sources"))
        {
            let thumbnails = thumbnails_from_sources(sources);
            if !thumbnails.is_empty() {
                return thumbnails;
            }
        }
    }
    if let Some(thumbnails) = header.get("avatar").map(thumbnails_of)
        && !thumbnails.is_empty()
    {
        return thumbnails;
    }
    metadata
        .and_then(|meta| meta.get("avatar"))
        .map(thumbnails_of)
        .unwrap_or_default()
}

fn banner_of(header: &Value) -> Option<Thumbnail> {
    if let Some(view) = page_header_view_model(header) {
        // System channels (e.g. auto-generated artist channels) render no banner at all; the view model
        // then has no `banner` member. That is absence, not an empty banner.
        let sources = view
            .get("banner")?
            .get("imageBannerViewModel")?
            .get("image")?
            .get("sources")?;
        return largest(thumbnails_from_sources(sources));
    }
    let thumbnails = header.get("banner").map(thumbnails_of)?;
    largest(thumbnails)
}

/// Metadata rows of the current page header: `metadata.contentMetadataViewModel.metadataRows[]`.
///
/// Returns one entry per row, each a list of `(text, has_link)` parts in document order.
fn header_metadata_rows(header: &Value) -> Vec<Vec<(String, bool)>> {
    let Some(rows) = page_header_view_model(header)
        .and_then(|view| view.get("metadata"))
        .and_then(|meta| meta.get("contentMetadataViewModel"))
        .and_then(|meta| meta.get("metadataRows"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for row in rows {
        let Some(parts) = row.get("metadataParts").and_then(Value::as_array) else {
            continue;
        };
        let mut texts = Vec::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(text_of) {
                let has_link = part
                    .get("text")
                    .and_then(|text| text.get("commandRuns"))
                    .is_some_and(|runs| !runs.is_null());
                texts.push((text, has_link));
            }
        }
        if !texts.is_empty() {
            out.push(texts);
        }
    }
    out
}

/// Counts stated by the channel header, without inventing a subscriber number.
///
/// The current header renders the subscriber and video counts as metadata *parts* whose wording is
/// localized ("1.24M subscribers" / "1.2K videos" / "Abonnenten"), so they cannot be identified by
/// keyword. Reference clients rely on position: `NewPipeExtractor` documents that "the subscriber count
/// is at the same position for all languages as of 07/07/2025" and reads `metadataParts[0]` of the last
/// row, treating a shorter row as "no subscriber count" (auto-generated artist channels render only the
/// video count).
///
/// Positional reading is fragile, so this function refuses to guess when the shape is ambiguous:
///
/// * exactly two count-bearing parts → first is subscribers, second is videos (marked *approximate* and
///   with the rendered text preserved, because the number came from a human-formatted label);
/// * exactly one count-bearing part → **neither** is set, and the ambiguity is recorded as a warning.
///
/// A count is never inferred from a label alone, and a channel that hides its subscriber count keeps
/// `None` rather than `0`.
fn header_counts(header: &Value, report: &mut ParseReport) -> (Option<Count>, Option<Count>) {
    // Legacy shape states both counts explicitly, so it needs no positional reading.
    let legacy_subscribers = header
        .get("subscriberCountText")
        .and_then(text_of)
        .or_else(|| header.get("subtitle").and_then(text_of))
        .as_deref()
        .and_then(count_from_text);
    let legacy_videos = header
        .get("videoCountText")
        .and_then(text_of)
        .as_deref()
        .and_then(count_from_text);
    if legacy_subscribers.is_some() || legacy_videos.is_some() {
        return (legacy_subscribers, legacy_videos);
    }

    let rows = header_metadata_rows(header);
    let Some(last) = rows.last() else {
        return (None, None);
    };
    let counted: Vec<Count> = last
        .iter()
        .filter_map(|(text, _)| count_from_text(text))
        .collect();
    match counted.len() {
        2 => (counted.first().cloned(), counted.get(1).cloned()),
        1 => {
            report.warn("channel_counts_ambiguous");
            (None, None)
        }
        _ => (None, None),
    }
}

/// Verification indicators rendered anywhere under the header.
///
/// Both badge shapes are read (`metadataBadgeRenderer` and `badgeViewModel`); the style string is kept
/// verbatim so that a client can distinguish "verified" from "artist" without Vydra asserting which
/// style means what.
fn verification_of(header: &Value) -> Vec<ChannelVerification> {
    let mut out: Vec<ChannelVerification> = Vec::new();
    let mut stack = vec![header];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 5_000 {
            break;
        }
        match node {
            Value::Object(map) => {
                for key in ["metadataBadgeRenderer", "badgeViewModel"] {
                    if let Some(badge) = map.get(key).filter(|value| value.is_object()) {
                        let style = badge
                            .get("style")
                            .and_then(Value::as_str)
                            .or_else(|| {
                                badge
                                    .get("badge")
                                    .and_then(|inner| inner.get("style"))
                                    .and_then(Value::as_str)
                            })
                            .unwrap_or("unknown");
                        let label = badge
                            .get("label")
                            .and_then(Value::as_str)
                            .or_else(|| {
                                badge
                                    .get("badge")
                                    .and_then(|inner| inner.get("label"))
                                    .and_then(Value::as_str)
                            })
                            .map(str::to_owned);
                        let candidate = ChannelVerification {
                            style: style.to_owned(),
                            label,
                        };
                        if !out.contains(&candidate) {
                            out.push(candidate);
                        }
                    }
                }
                // Children are pushed in reverse so that the LIFO stack yields them in document order:
                // tabs and verification badges must be reported in the order the page renders them.
                let children: Vec<&Value> = map
                    .values()
                    .filter(|value| value.is_object() || value.is_array())
                    .collect();
                stack.extend(children.into_iter().rev());
            }
            Value::Array(items) => stack.extend(items.iter().rev().filter(|item| item.is_object())),
            _ => {}
        }
    }
    out
}

/// Tabs rendered on the page, in document order.
fn tabs_of(payload: &Value) -> Vec<ChannelTab> {
    let mut out = Vec::new();
    let mut stack = vec![payload];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 20_000 {
            break;
        }
        match node {
            Value::Object(map) => {
                if let Some(tab) = map.get("tabRenderer").filter(|value| value.is_object()) {
                    if let Some(title) = tab.get("title").and_then(text_of) {
                        let slug = tab_slug(&title);
                        let candidate = ChannelTab {
                            title,
                            slug,
                            selected: tab
                                .get("selected")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        };
                        if !out.contains(&candidate) {
                            out.push(candidate);
                        }
                    }
                    // A tab's content is a list of its own; do not descend into it.
                    continue;
                }
                // Children are pushed in reverse so that the LIFO stack yields them in document order:
                // tabs and verification badges must be reported in the order the page renders them.
                let children: Vec<&Value> = map
                    .values()
                    .filter(|value| value.is_object() || value.is_array())
                    .collect();
                stack.extend(children.into_iter().rev());
            }
            Value::Array(items) => stack.extend(items.iter().rev().filter(|item| item.is_object())),
            _ => {}
        }
    }
    out
}

/// The `params` blob of the tab that lists the channel's videos, relayed verbatim.
///
/// This is the only supported source of tab `params` (see `request::BrowseRequest::channel_tab`): the
/// value is copied from `tabRenderer.endpoint.browseEndpoint.params` on the channel page itself, so no
/// protobuf blob is encoded by Vydra and none is hard-coded.
pub fn videos_tab_params(payload: &Value) -> Option<String> {
    let mut stack = vec![payload];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 20_000 {
            return None;
        }
        match node {
            Value::Object(map) => {
                if let Some(tab) = map.get("tabRenderer").filter(|value| value.is_object()) {
                    let title = tab.get("title").and_then(text_of).unwrap_or_default();
                    if tab_slug(&title).as_deref() == Some("videos") {
                        let params = tab
                            .get("endpoint")
                            .and_then(|endpoint| endpoint.get("browseEndpoint"))
                            .and_then(|browse| browse.get("params"))
                            .and_then(Value::as_str)
                            .filter(|params| super::request::is_relayable_params(params));
                        if let Some(params) = params {
                            return Some(params.to_owned());
                        }
                    }
                    continue;
                }
                // Children are pushed in reverse so that the LIFO stack yields them in document order:
                // tabs and verification badges must be reported in the order the page renders them.
                let children: Vec<&Value> = map
                    .values()
                    .filter(|value| value.is_object() || value.is_array())
                    .collect();
                stack.extend(children.into_iter().rev());
            }
            Value::Array(items) => stack.extend(items.iter().rev().filter(|item| item.is_object())),
            _ => {}
        }
    }
    None
}

/// Page-level `alerts[]` messages, as `(type, text)`.
fn alerts_of(payload: &Value) -> Vec<(String, String)> {
    let Some(alerts) = payload.get("alerts").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for alert in alerts {
        let renderer = alert
            .get("alertRenderer")
            .or_else(|| alert.get("alertWithButtonRenderer"));
        let Some(renderer) = renderer else { continue };
        let kind = renderer
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_owned();
        if let Some(text) = renderer.get("text").and_then(text_of) {
            out.push((kind, text));
        }
    }
    out
}

/// Map a page-level alert into a provider error.
///
/// The mapping is deliberately narrow: only the two verdicts that upstream states in words — "does not
/// exist" and "private" — are translated, and only for `warning`/`error` alerts. Anything else keeps the
/// page's own text in the error message instead of being encoded as a reason Vydra cannot support.
fn alert_error(alerts: &[(String, String)]) -> Option<ProviderError> {
    let (_, text) = alerts.iter().find(|(kind, _)| {
        kind.eq_ignore_ascii_case("ERROR") || kind.eq_ignore_ascii_case("WARNING")
    })?;
    let lowered = text.to_lowercase();
    if lowered.contains("does not exist")
        || lowered.contains("no longer available")
        || lowered.contains("unavailable")
    {
        return Some(ProviderError::NotFound {
            id: "browse".to_owned(),
            message: text.clone(),
        });
    }
    if lowered.contains("private") {
        return Some(ProviderError::Unavailable {
            reason: UnavailabilityReason::Private,
            message: text.clone(),
        });
    }
    Some(ProviderError::Unavailable {
        reason: UnavailabilityReason::Other,
        message: text.clone(),
    })
}

/// Normalize a `vanityChannelUrl`/`channelUrl` into a bare handle.
fn handle_of(url: &str) -> Option<String> {
    let path = url.rsplit('/').next()?;
    path.strip_prefix('@')
        .map(str::to_owned)
        .filter(|handle| !handle.is_empty())
}

/// Map a channel browse response.
///
/// `requested` is the id or handle the caller asked for; the document's id is the canonical
/// `externalId`, and a response whose canonical id does not correspond to the request is an error, not a
/// silently different resource. A *redirect* (localized channels, auto-generated "Movies and Shows"
/// channels with several ids) is accepted and recorded: the canonical id is kept and the substitution is
/// written into `evidence.conflicts` so the served document explains why its id differs from the request.
pub fn map_channel(
    payload: &Value,
    requested: &ChannelRef,
    report: &mut ParseReport,
    provenance: Provenance,
) -> Result<ChannelDocument, ProviderError> {
    let mut evidence = MetadataEvidence::default();

    let metadata = channel_metadata(payload);
    if let Some(metadata) = metadata {
        evidence.section(vydra_domain::FieldSource::ChannelMetadata);
        let external = metadata
            .get("externalId")
            .and_then(Value::as_str)
            .and_then(|raw| ChannelId::parse(raw).ok());
        let Some(canonical) = external else {
            report.structural_problem("channel_external_id_missing");
            return Err(ProviderError::Parse {
                message: "channel page without a valid externalId".to_owned(),
            });
        };

        match requested {
            ChannelRef::Id(id) if id != &canonical => {
                // Documented redirect: keep the canonical id (the page is the authority) and say so.
                evidence.conflict(vydra_domain::FieldConflict {
                    field: "id".to_owned(),
                    kept: vydra_domain::FieldSource::ChannelMetadata,
                    kept_value: canonical.as_str().to_owned(),
                    rejected: vydra_domain::FieldSource::Request,
                    rejected_value: id.as_str().to_owned(),
                });
                report.warn("channel_id_redirected");
            }
            ChannelRef::Handle(handle) => {
                let claimed = metadata
                    .get("vanityChannelUrl")
                    .and_then(Value::as_str)
                    .and_then(handle_of);
                if let Some(claimed) = claimed
                    && !claimed.eq_ignore_ascii_case(handle.as_str().trim_start_matches('@'))
                {
                    report.structural_problem("channel_handle_mismatch");
                    return Err(ProviderError::Parse {
                        message: "channel page does not belong to the requested handle".to_owned(),
                    });
                }
            }
            ChannelRef::Id(_) => {}
        }

        let header = channel_header(payload);
        if header.is_some() {
            evidence.section(vydra_domain::FieldSource::ChannelHeader);
        }
        let tabs = tabs_of(payload);
        if !tabs.is_empty() {
            evidence.section(vydra_domain::FieldSource::Tabs);
        }

        let metadata_title = metadata.get("title").and_then(text_of);
        let header_title = header
            .and_then(|header| first_text(header, &["title", "pageTitle"]))
            .or_else(|| {
                page_header_view_model(header?)
                    .and_then(|view| view.get("title"))
                    .and_then(|title| {
                        title
                            .get("dynamicTextViewModel")
                            .and_then(|dynamic| dynamic.get("text"))
                            .and_then(text_of)
                    })
            });
        let title = match (metadata_title, header_title) {
            (Some(metadata_title), Some(header_title)) => {
                if metadata_title != header_title {
                    evidence.conflict(vydra_domain::FieldConflict {
                        field: "title".to_owned(),
                        kept: vydra_domain::FieldSource::ChannelMetadata,
                        kept_value: metadata_title.clone(),
                        rejected: vydra_domain::FieldSource::ChannelHeader,
                        rejected_value: header_title,
                    });
                }
                metadata_title
            }
            (Some(metadata_title), None) => metadata_title,
            (None, Some(header_title)) => {
                evidence.fallback("title", vydra_domain::FieldSource::ChannelHeader);
                header_title
            }
            (None, None) => {
                report.missing_field("channelMetadataRenderer", "title");
                return Err(ProviderError::Parse {
                    message: "channel page without a title".to_owned(),
                });
            }
        };

        let (subscriber_count, video_count) = match header {
            Some(header) => header_counts(header, report),
            None => (None, None),
        };

        let avatar = match header {
            Some(header) => avatar_of(header, Some(metadata)),
            None => metadata.get("avatar").map(thumbnails_of).unwrap_or_default(),
        };
        let banner = header.and_then(banner_of);
        let verification = header.map(verification_of).unwrap_or_default();

        let description = metadata
            .get("description")
            .and_then(text_of)
            .or_else(|| header.and_then(|header| first_text(header, &["description"])));

        let handle = metadata
            .get("vanityChannelUrl")
            .and_then(Value::as_str)
            .and_then(handle_of);

        return Ok(ChannelDocument {
            summary: ChannelSummary {
                id: canonical,
                title,
                handle,
                description,
                thumbnails: avatar,
                subscriber_count,
                video_count,
            },
            // The page states the channel exists by rendering its metadata. `Unavailable` is not
            // reachable here because those responses are refused above as errors; `Unknown` would hide
            // the fact that the page did answer.
            availability: Availability::Available,
            banner,
            links: Vec::new(),
            joined_at: None,
            country: None,
            total_view_count: None,
            tabs,
            verification,
            evidence,
            provenance,
        });
    }

    // No metadata: the page is either an error page or a shape we do not know.
    let alerts = alerts_of(payload);
    if let Some(error) = alert_error(&alerts) {
        let mut error = error;
        if let ProviderError::NotFound { id, .. } = &mut error {
            *id = requested.as_str().to_owned();
        }
        report.structural_problem("channel_page_alert");
        return Err(error);
    }

    if payload.get("header").and_then(|h| h.get("channelAgeGateRenderer")).is_some() {
        report.structural_problem("channel_age_gate");
        return Err(ProviderError::Unavailable {
            reason: UnavailabilityReason::AgeRestricted,
            message: "channel is age gated and this client is not signed in".to_owned(),
        });
    }

    report.structural_problem("channel_metadata_missing");
    Err(ProviderError::Parse {
        message: "browse response is not a channel page".to_owned(),
    })
}

fn video_items(
    renderers: Vec<Renderer<'_>>,
    report: &mut ParseReport,
) -> (Vec<vydra_domain::VideoSummary>, u32) {
    let mut items: Vec<vydra_domain::VideoSummary> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0u32;
    for renderer in renderers {
        match super::map::search_item_from_renderer(&renderer, report) {
            Some(SearchItem::Video(video)) => {
                if seen.insert(video.id.as_str().to_owned()) {
                    items.push(video);
                } else {
                    // The same row twice in one page is an upstream anomaly, not a playlist feature.
                    duplicates += 1;
                }
            }
            Some(other) => {
                // A channel/playlist row inside a video listing is content this endpoint cannot serve.
                let _ = other;
                report.unmapped_result("non_video_row");
            }
            None => {}
        }
    }
    (items, duplicates)
}

/// Next page token, taken **only** from a real page cursor.
fn next_page_token(payload: &Value, report: &mut ParseReport) -> Option<String> {
    if let Some(token) = find_page_cursor(payload) {
        return Some(token);
    }
    // Something else carried a token: a chip, a sort menu, or an older envelope. Using it would change
    // the result set instead of paging it, so it is dropped and reported.
    if find_continuation(payload).is_some() {
        report.warn("continuation_without_page_cursor");
    }
    None
}

/// Map a channel videos page (first page or continuation).
pub fn map_channel_videos(
    payload: &Value,
    limit: u16,
    report: &mut ParseReport,
    provenance: Provenance,
) -> ChannelVideoList {
    let renderers = collect_renderers(payload, report);
    let (mut items, duplicates_removed) = video_items(renderers, report);
    if items.is_empty() && duplicates_removed == 0 {
        report.structural_problem("no_video_rows");
    }
    if items.len() > usize::from(limit) {
        items.truncate(usize::from(limit));
        report.warn("page_truncated");
    }
    let next_cursor = next_page_token(payload, report);
    ChannelVideoList {
        items,
        next_cursor,
        duplicates_removed,
        provenance,
    }
}

/// Map a `playlistVideoRenderer` into an entry, keeping its position whatever its playability.
fn playlist_item_from_renderer(
    renderer: &Value,
    report: &mut ParseReport,
) -> Option<PlaylistItem> {
    let position = renderer
        .get("index")
        .and_then(text_of)
        .and_then(|text| text.trim().parse::<u32>().ok());

    let title = renderer.get("title").and_then(text_of);
    let Some(title) = title else {
        report.missing_field("playlistVideoRenderer", "title");
        return None;
    };

    let raw_id = renderer.get("videoId").and_then(Value::as_str);
    let video_id = match raw_id {
        Some(raw) => match VideoId::parse(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                // Fail closed: the row stays (so positions do not shift) but carries no video reference.
                report.warn("playlist_entry_id_invalid");
                None
            }
        },
        None => None,
    };

    // `isPlayable` is the row's own verdict. Its absence is `Unknown`, never `Available`: the newer
    // `lockupViewModel` shape has no such field, and assuming playability is exactly the mistake the
    // Phase 5A audit found on the player path.
    let availability = match renderer.get("isPlayable").and_then(Value::as_bool) {
        Some(true) => Availability::Available,
        Some(false) => Availability::from_reason(UnavailabilityReason::Other),
        None => Availability::Unknown,
    };

    let duration_seconds = renderer
        .get("lengthSeconds")
        // Upstream has served this field as both a number and a numeric string.
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.trim().parse::<u64>().ok()))
        })
        .and_then(|seconds| u32::try_from(seconds).ok())
        .or_else(|| {
            renderer
                .get("lengthText")
                .and_then(text_of)
                .and_then(|text| parse_clock_duration(&text))
        });

    let thumbnails = renderer
        .get("thumbnail")
        .map(thumbnails_of)
        .unwrap_or_default();

    Some(PlaylistItem {
        position,
        video_id,
        title,
        channel_id: channel_id_of(renderer, &["shortBylineText", "longBylineText"]),
        channel_title: first_text(renderer, &["shortBylineText", "longBylineText"]),
        duration_seconds,
        thumbnails,
        availability,
        source: PlaylistEntrySource::PlaylistVideo,
    })
}

fn playlist_items(
    renderers: Vec<Renderer<'_>>,
    limit: u16,
    report: &mut ParseReport,
) -> (Vec<PlaylistItem>, u32) {
    let mut items: Vec<PlaylistItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut repeated = 0u32;

    for renderer in renderers {
        match renderer.key {
            "playlistVideoRenderer" => {}
            "lockupViewModel" => {
                // Readable in principle, but the lockup shape carries neither `isPlayable` nor `index`;
                // mapping it would mean inventing both. Counted as unmapped content instead.
                report.unmapped_result("lockupViewModel");
                continue;
            }
            _ => continue,
        }
        let Some(item) = playlist_item_from_renderer(renderer.value, report) else {
            continue;
        };
        if let Some(id) = &item.video_id
            && !seen.insert(id.as_str().to_owned())
        {
            // Legitimate: the same video may be in a playlist twice. Counted, never dropped.
            repeated += 1;
        }
        items.push(item);
    }

    if items.len() > usize::from(limit) {
        items.truncate(usize::from(limit));
        report.warn("page_truncated");
    }
    (items, repeated)
}

/// Header/sidebar facts shared by the playlist document and its listing pages.
struct PlaylistFacts {
    title: Option<String>,
    description: Option<String>,
    item_count: Option<Count>,
    view_count: Option<Count>,
    last_updated_text: Option<String>,
    channel_id: Option<ChannelId>,
    channel_title: Option<String>,
    thumbnails: Vec<Thumbnail>,
    sidebar_stats: Vec<Count>,
}

fn playlist_facts(payload: &Value, evidence: &mut MetadataEvidence) -> PlaylistFacts {
    let mut facts = PlaylistFacts {
        title: None,
        description: None,
        item_count: None,
        view_count: None,
        last_updated_text: None,
        channel_id: None,
        channel_title: None,
        thumbnails: Vec::new(),
        sidebar_stats: Vec::new(),
    };

    if let Some(metadata) = payload.get("metadata").and_then(|meta| meta.get("playlistMetadataRenderer")) {
        evidence.section(vydra_domain::FieldSource::PlaylistMetadata);
        facts.title = metadata.get("title").and_then(text_of);
        facts.description = metadata.get("description").and_then(text_of);
    }

    if let Some(header) = payload.get("header").and_then(|header| header.get("playlistHeaderRenderer")) {
        evidence.section(vydra_domain::FieldSource::PlaylistHeader);
        if facts.title.is_none() {
            facts.title = header.get("title").and_then(text_of);
            if facts.title.is_some() {
                evidence.fallback("title", vydra_domain::FieldSource::PlaylistHeader);
            }
        }
        if facts.description.is_none() {
            facts.description = header.get("description").and_then(text_of);
            if facts.description.is_some() {
                evidence.fallback("description", vydra_domain::FieldSource::PlaylistHeader);
            }
        }
        // `numVideosText` is rendered text ("1,204 videos"), so the count it yields is approximate.
        facts.item_count = header
            .get("numVideosText")
            .and_then(text_of)
            .as_deref()
            .and_then(count_from_text);
        facts.view_count = header
            .get("viewCountText")
            .and_then(text_of)
            .as_deref()
            .and_then(count_from_text);
        facts.channel_id = channel_id_of(header, &["ownerText", "shortBylineText"]);
        facts.channel_title = first_text(header, &["ownerText", "shortBylineText"]);
        let banner = header
            .get("playlistHeaderBanner")
            .and_then(|banner| banner.get("heroPlaylistThumbnailRenderer"))
            .and_then(|thumbnail| thumbnail.get("thumbnail"))
            .map(thumbnails_of)
            .unwrap_or_default();
        facts.thumbnails = banner;
    }

    if let Some(sidebar) = payload
        .get("sidebar")
        .and_then(|sidebar| sidebar.get("playlistSidebarPrimaryInfoRenderer"))
    {
        evidence.section(vydra_domain::FieldSource::PlaylistSidebar);
        if facts.title.is_none() {
            facts.title = sidebar.get("title").and_then(text_of);
            if facts.title.is_some() {
                evidence.fallback("title", vydra_domain::FieldSource::PlaylistSidebar);
            }
        }
        if let Some(stats) = sidebar.get("stats").and_then(Value::as_array) {
            for stat in stats {
                if let Some(count) = text_of(stat).as_deref().and_then(count_from_text) {
                    facts.sidebar_stats.push(count);
                }
            }
            // The last stat is the "last updated" line; it is a date, not a count, so it is kept as the
            // page's own wording rather than parsed into a timestamp of unknown timezone.
            if facts.last_updated_text.is_none()
                && let Some(text) = stats.last().and_then(text_of)
                && count_from_text(&text).is_none()
                && text.chars().any(|c| c.is_ascii_digit())
            {
                facts.last_updated_text = Some(text);
            }
        }
        if facts.last_updated_text.is_none() {
            facts.last_updated_text = sidebar.get("dateText").and_then(text_of);
        }
        if facts.channel_id.is_none() {
            facts.channel_id = payload
                .get("sidebar")
                .and_then(|sidebar| sidebar.get("playlistSidebarSecondaryInfoRenderer"))
                .and_then(|secondary| secondary.get("videoOwner"))
                .and_then(|owner| owner.get("videoOwnerRenderer"))
                .and_then(|owner| channel_id_of(owner, &["title"]));
        }
        if facts.thumbnails.is_empty() {
            facts.thumbnails = sidebar
                .get("thumbnailRenderer")
                .and_then(|renderer| {
                    renderer
                        .get("playlistVideoThumbnailRenderer")
                        .or_else(|| renderer.get("playlistCustomThumbnailRenderer"))
                })
                .and_then(|renderer| renderer.get("thumbnail"))
                .map(thumbnails_of)
                .unwrap_or_default();
        }
    }

    // Sidebar stats are ordered "videos, views, updated"; fill holes from them only when the header did
    // not state the fact, and only from the position the reference clients document.
    if facts.item_count.is_none()
        && let Some(count) = facts.sidebar_stats.first()
    {
        facts.item_count = Some(count.clone());
        evidence.fallback("item_count", vydra_domain::FieldSource::PlaylistSidebar);
    }
    if facts.view_count.is_none()
        && let Some(count) = facts.sidebar_stats.get(1)
    {
        facts.view_count = Some(count.clone());
        evidence.fallback("view_count", vydra_domain::FieldSource::PlaylistSidebar);
    }

    facts
}

/// Map a playlist browse response (`browseId = VL<id>`).
pub fn map_playlist(
    payload: &Value,
    expected: &PlaylistId,
    limit: u16,
    report: &mut ParseReport,
    provenance: Provenance,
) -> Result<PlaylistDocument, ProviderError> {
    let mut evidence = MetadataEvidence::default();
    let facts = playlist_facts(payload, &mut evidence);

    let Some(title) = facts.title.clone() else {
        let alerts = alerts_of(payload);
        if let Some(mut error) = alert_error(&alerts) {
            if let ProviderError::NotFound { id, .. } = &mut error {
                *id = expected.as_str().to_owned();
            }
            report.structural_problem("playlist_page_alert");
            return Err(error);
        }
        report.structural_problem("playlist_header_missing");
        return Err(ProviderError::Parse {
            message: "browse response is not a playlist page".to_owned(),
        });
    };

    let renderers = collect_renderers(payload, report);
    let (items, repeated_entries) = playlist_items(renderers, limit, report);
    if items.is_empty() && facts.item_count.is_none() {
        report.warn("playlist_entries_missing");
    }
    if !items.is_empty() {
        evidence.section(vydra_domain::FieldSource::PlaylistVideoList);
    }
    let next_cursor = next_page_token(payload, report);

    Ok(PlaylistDocument {
        summary: PlaylistSummary {
            id: expected.clone(),
            title,
            channel_id: facts.channel_id,
            channel_title: facts.channel_title,
            description: facts.description,
            item_count: facts.item_count,
            thumbnails: facts.thumbnails,
        },
        availability: Availability::Available,
        items,
        next_cursor,
        view_count: facts.view_count,
        last_updated_text: facts.last_updated_text,
        repeated_entries: (repeated_entries > 0).then_some(repeated_entries),
        evidence,
        provenance,
    })
}

/// Map a playlist entries page (first page or continuation).
pub fn map_playlist_videos(
    payload: &Value,
    limit: u16,
    report: &mut ParseReport,
    provenance: Provenance,
) -> PlaylistVideoList {
    let renderers = collect_renderers(payload, report);
    let (items, repeated) = playlist_items(renderers, limit, report);
    if items.is_empty() {
        report.structural_problem("no_playlist_rows");
    }
    let next_cursor = next_page_token(payload, report);
    PlaylistVideoList {
        items,
        next_cursor,
        repeated_entries: (repeated > 0).then_some(repeated),
        provenance,
    }
}
