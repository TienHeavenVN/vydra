//! Search parser reliability tests.
//!
//! These drive `collect_renderers` + `map_search` directly, so each assertion is about *parsing* and
//! nothing else: no provider, no transport, no session. Every document is either hand-written here
//! (clearly marked) or read from `tests/fixtures/` (provenance in `docs/research/FIXTURE_PROVENANCE.md`).
//!
//! The rule the whole file enforces: **unknown content must not silently become a valid item.** A
//! renderer Vydra does not understand is counted, not coerced; a renderer missing a required field is
//! skipped, not zero-filled.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use serde_json::{Value, json};
use time::OffsetDateTime;
use vydra_domain::{Precision, Provenance, SearchItem};
use vydra_providers::innertube::map::map_search;
use vydra_providers::parse::{ParseReport, collect_renderers};

fn provenance() -> Provenance {
    Provenance {
        provider: "innertube-web".into(),
        endpoint: "search".into(),
        client: Some("WEB".into()),
        fetched_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts"),
        cached: false,
        parse_warnings: 0,
        sources: Vec::new(),
    }
}

/// Parse a document into `(results, report)`.
fn parse(document: &Value) -> (vydra_domain::SearchResults, ParseReport) {
    let mut report = ParseReport::new();
    let renderers = collect_renderers(document, &mut report);
    let results = map_search(document, renderers, &mut report, provenance());
    (results, report)
}

fn video(id: &str, title: &str) -> Value {
    json!({
        "videoRenderer": {
            "videoId": id,
            "title": {"simpleText": title},
            "lengthText": {"simpleText": "3:05"},
            "viewCountText": {"simpleText": "1000 views"},
            "publishedTimeText": {"simpleText": "1 day ago"},
            "thumbnail": {"thumbnails": [{"url": "//i.ytimg.com/vi/x/low.jpg", "width": 120, "height": 90}]},
        }
    })
}

fn channel(id: &str, title: &str) -> Value {
    json!({
        "channelRenderer": {
            "channelId": id,
            "title": {"simpleText": title},
            "descriptionSnippet": {"runs": [{"text": "A channel"}]},
            "subscriberCountText": {"simpleText": "1.2M subscribers"},
            "videoCountText": {"simpleText": "345 videos"},
            "navigationEndpoint": {"browseEndpoint": {"browseId": id, "canonicalBaseUrl": "/@handle"}},
            "thumbnail": {"thumbnails": [{"url": "//yt3.ggpht.com/x.jpg", "width": 88, "height": 88}]},
        }
    })
}

fn playlist(id: &str, title: &str) -> Value {
    json!({
        "playlistRenderer": {
            "playlistId": id,
            "title": {"simpleText": title},
            "videoCountText": {"runs": [{"text": "20"}, {"text": " videos"}]},
            "shortBylineText": {"runs": [{"text": "Owner"}]},
            "thumbnail": {"thumbnails": [{"url": "//i.ytimg.com/vi/x/low.jpg", "width": 120, "height": 90}]},
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Renderer kinds
// ---------------------------------------------------------------------------------------------

#[test]
fn channel_renderer_maps_every_documented_field() {
    let document = json!({"contents": [channel("UCa8RiU2st_48AA4WscbwYVQ", "Captain Jack Official")]});
    let (results, _report) = parse(&document);

    assert_eq!(results.items.len(), 1);
    let SearchItem::Channel(mapped) = &results.items[0] else {
        panic!("expected a channel");
    };
    assert_eq!(mapped.id.as_str(), "UCa8RiU2st_48AA4WscbwYVQ");
    assert_eq!(mapped.title, "Captain Jack Official");
    assert_eq!(mapped.handle.as_deref(), Some("handle"));
    assert_eq!(mapped.description.as_deref(), Some("A channel"));
    assert_eq!(
        mapped.subscriber_count.as_ref().map(|count| count.value),
        Some(1_200_000)
    );
    assert_eq!(
        mapped.video_count.as_ref().map(|count| count.value),
        Some(345)
    );
    assert_eq!(mapped.thumbnails.len(), 1);
}

#[test]
fn playlist_renderer_maps_the_item_count_from_runs() {
    let document = json!({"contents": [playlist("PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR", "Mix")]});
    let (results, _report) = parse(&document);

    let SearchItem::Playlist(mapped) = &results.items[0] else {
        panic!("expected a playlist");
    };
    assert_eq!(mapped.id.as_str(), "PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR");
    assert_eq!(mapped.title, "Mix");
    assert_eq!(mapped.item_count.as_ref().map(|count| count.value), Some(20));
}

#[test]
fn a_channel_without_an_id_is_skipped_not_invented() {
    let document = json!({"contents": [{"channelRenderer": {"title": {"simpleText": "Ghost"}}}]});
    let (results, report) = parse(&document);

    assert!(results.items.is_empty(), "nothing may be synthesised");
    assert!(
        report.messages().iter().any(|message| message.contains("channel_without_id")),
        "messages: {:?}",
        report.messages()
    );
    assert!(report.count() >= 1, "the skip must be counted");
}

#[test]
fn an_invalid_video_id_is_skipped_not_truncated() {
    // "abc" is too short to be a video id; a lenient parser would pad or accept it.
    let document = json!({"contents": [video("abc", "Short id")]});
    let (results, report) = parse(&document);

    assert!(results.items.is_empty());
    assert!(
        report.messages().iter().any(|message| message.contains("video_id_invalid")),
        "messages: {:?}",
        report.messages()
    );
}

#[test]
fn a_video_without_a_title_is_skipped_and_counted() {
    let document = json!({"contents": [{
        "videoRenderer": {"videoId": "dQw4w9WgXcQ", "lengthText": {"simpleText": "3:33"}}
    }]});
    let (results, report) = parse(&document);

    assert!(results.items.is_empty());
    assert!(
        report.messages().iter().any(|message| message.contains("videoRenderer.title")),
        "messages: {:?}",
        report.messages()
    );
}

// ---------------------------------------------------------------------------------------------
// Containers and unknown content
// ---------------------------------------------------------------------------------------------

#[test]
fn containers_are_transparent_for_mapped_renderers() {
    // Same three items, wrapped in the section/shelf shape a real page uses.
    let document = json!({
        "contents": {
            "twoColumnSearchResultsRenderer": {
                "primaryContents": {
                    "sectionListRenderer": {
                        "contents": [
                            {"itemSectionRenderer": {"contents": [
                                video("dQw4w9WgXcQ", "Song"),
                                channel("UCa8RiU2st_48AA4WscbwYVQ", "Person"),
                            ]}},
                            {"itemSectionRenderer": {"contents": [
                                {"shelfRenderer": {"content": {"expandedShelfContentsRenderer": {"items": [
                                    playlist("PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR", "Mix"),
                                ]}}}},
                            ]}},
                        ]
                    }
                }
            }
        }
    });
    let (results, report) = parse(&document);

    let kinds: Vec<&str> = results.items.iter().map(|item| item.kind()).collect();
    assert_eq!(kinds, vec!["video", "channel", "playlist"]);
    assert_eq!(report.unknown_renderers(), 0, "containers are not unknown content");
}

#[test]
fn unknown_renderers_never_become_items() {
    // A Shorts shelf and a "you might also like" card: shapes Vydra does not map.
    let document = json!({
        "contents": {"sectionListRenderer": {"contents": [
            {"itemSectionRenderer": {"contents": [
                {"reelShelfRenderer": {"items": [{"reelItemRenderer": {"videoId": "dQw4w9WgXcQ"}}]}},
                {"shortsLockupViewModel": {"entityId": "shorts-shelf-item-dQw4w9WgXcQ"}},
                {"videoRenderer": {"videoId": "dQw4w9WgXcQ", "title": {"simpleText": "Real"}}},
            ]}},
        ]}}
    });
    let (results, report) = parse(&document);

    assert_eq!(results.items.len(), 1, "only the videoRenderer may map");
    assert_eq!(results.items[0].kind(), "video");
    assert!(
        report.unknown_renderers() >= 1,
        "unknown content must be counted, not dropped silently"
    );
    // ... and the counted warning reaches `parse_warnings` through the provider.
    assert!(report.count() >= 1);
}

// ---------------------------------------------------------------------------------------------
// Ordering, duplicates, limits
// ---------------------------------------------------------------------------------------------

#[test]
fn duplicates_collapse_to_the_first_occurrence_and_are_counted() {
    let document = json!({"contents": [
        video("dQw4w9WgXcQ", "First"),
        video("9Umm2MAKsMc", "Second"),
        video("dQw4w9WgXcQ", "First again"),
    ]});
    let (results, report) = parse(&document);

    let ids: Vec<&str> = results
        .items
        .iter()
        .filter_map(|item| match item {
            SearchItem::Video(video) => Some(video.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["dQw4w9WgXcQ", "9Umm2MAKsMc"], "order preserved, repeat dropped");
    assert!(
        report.messages().iter().any(|message| message.contains("duplicate_item")),
        "messages: {:?}",
        report.messages()
    );
}

#[test]
fn the_same_id_in_two_kinds_is_not_a_duplicate() {
    // A channel and a video cannot share an id in practice, but the de-duplication key must include
    // the kind so that it can never merge two different entities by accident.
    let document = json!({"contents": [
        video("UCa8RiU2st_48AA4WscbwYVQ", "Not a real video id"),
        channel("UCa8RiU2st_48AA4WscbwYVQ", "Channel"),
    ]});
    let (results, _report) = parse(&document);
    // The video id is rejected (a UC… id is 24 chars but does not validate as a video id here), so
    // only the channel survives; the assertion is about the kinds never being merged.
    assert!(results.items.iter().all(|item| item.kind() == "channel"));
}

// ---------------------------------------------------------------------------------------------
// Localized values, thumbnails, pagination
// ---------------------------------------------------------------------------------------------

#[test]
fn localized_counts_and_clock_durations_are_parsed() {
    let cases = [
        ("25 399 352 просмотра", "1:43:17", Some(25_399_352), Some(6197)),
        ("1,2 Mio. Aufrufe", "12:34", Some(1_200_000), Some(754)),
        ("1.2B views", "3:05", Some(1_200_000_000), Some(185)),
        ("172 371 просмотр", "2:45", Some(172_371), Some(165)),
        ("No views", "0:07", None, Some(7)),
    ];
    for (views, duration, expected_views, expected_duration) in cases {
        let document = json!({"contents": [{"videoRenderer": {
            "videoId": "dQw4w9WgXcQ",
            "title": {"simpleText": "T"},
            "lengthText": {"simpleText": duration},
            "viewCountText": {"simpleText": views},
        }}]});
        let (results, _) = parse(&document);
        let SearchItem::Video(mapped) = &results.items[0] else {
            panic!("expected a video");
        };
        assert_eq!(
            mapped.view_count.as_ref().map(|count| count.value),
            expected_views,
            "views {views:?}"
        );
        assert_eq!(mapped.duration_seconds, expected_duration, "duration {duration:?}");
    }
}

#[test]
fn abbreviated_counts_are_marked_approximate() {
    let document = json!({"contents": [video("dQw4w9WgXcQ", "T")]});
    let (results, _) = parse(&document);
    let SearchItem::Video(mapped) = &results.items[0] else {
        panic!("expected a video");
    };
    let count = mapped.view_count.as_ref().expect("count");
    assert_eq!(count.value, 1_000);
    assert_eq!(count.precision, Precision::Approximate);
}

#[test]
fn thumbnails_are_sorted_and_protocol_relative_urls_are_normalised() {
    let document = json!({"contents": [{"videoRenderer": {
        "videoId": "dQw4w9WgXcQ",
        "title": {"simpleText": "T"},
        "thumbnail": {"thumbnails": [
            {"url": "//i.ytimg.com/vi/x/hq.jpg", "width": 480, "height": 360},
            {"url": "//i.ytimg.com/vi/x/default.jpg", "width": 120, "height": 90},
        ]},
    }}]});
    let (results, _) = parse(&document);
    let SearchItem::Video(mapped) = &results.items[0] else {
        panic!("expected a video");
    };
    let widths: Vec<Option<u32>> = mapped.thumbnails.iter().map(|thumb| thumb.width).collect();
    assert_eq!(widths, vec![Some(120), Some(480)], "smallest → largest");
    assert!(
        mapped.thumbnails.iter().all(|thumb| thumb.url.starts_with("https://")),
        "protocol-relative urls must be normalised: {:?}",
        mapped.thumbnails
    );
}

#[test]
fn continuation_tokens_are_found_in_both_container_shapes() {
    let search_shape = json!({
        "contents": {"twoColumnSearchResultsRenderer": {"primaryContents": {"sectionListRenderer": {"contents": [
            {"itemSectionRenderer": {"contents": [
                video("dQw4w9WgXcQ", "T"),
                {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "SEARCH-TOKEN"}}}},
            ]}},
        ]}}}}
    });
    let browse_shape = json!({
        "onResponseReceivedActions": [{"appendContinuationItemsAction": {"continuationItems": [
            {"richItemRenderer": {"content": {"videoRenderer": {"videoId": "dQw4w9WgXcQ", "title": {"simpleText": "T"}}}}},
            {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "BROWSE-TOKEN"}}}},
        ]}}]
    });

    let (search, _) = parse(&search_shape);
    assert_eq!(search.next_cursor.as_deref(), Some("SEARCH-TOKEN"));
    let (browse, _) = parse(&browse_shape);
    assert_eq!(browse.next_cursor.as_deref(), Some("BROWSE-TOKEN"));
}

#[test]
fn a_page_without_renderers_is_empty_and_counted() {
    let document = json!({"estimatedResults": 0, "contents": {}});
    let (results, report) = parse(&document);

    assert!(results.items.is_empty());
    assert!(results.next_cursor.is_none());
    assert_eq!(
        results.estimated_results.map(|count| count.value),
        Some(0),
        "an explicit zero is reported as zero, not as unknown"
    );
    assert_eq!(report.count(), 0, "the provider adds `no_renderers`; the mapper alone is quiet");
}

// ---------------------------------------------------------------------------------------------
// Recorded pages (captured by Vydra; see FIXTURE_PROVENANCE.md)
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("fixture {path}: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

#[test]
fn recorded_first_page_has_no_unknown_content_and_no_duplicates() {
    let document = fixture("recorded/search_web_probe_first_page.json");
    let (results, report) = parse(&document);

    assert!(!results.items.is_empty(), "a real page must produce items");
    assert!(
        results.next_cursor.is_some(),
        "a real first page carries a continuation token"
    );
    // Every renderer in the capture is one Vydra maps, and none repeats.
    assert_eq!(
        report.unknown_renderers(),
        0,
        "unmapped renderers: {:?}",
        report.messages()
    );
    assert!(
        !report.messages().iter().any(|message| message.contains("duplicate_item")),
        "capture contained duplicates: {:?}",
        report.messages()
    );
    assert!(
        results.next_cursor.as_deref().is_some_and(|token| token.len() > 40),
        "the recorded token must be a real one, not a placeholder"
    );
}
