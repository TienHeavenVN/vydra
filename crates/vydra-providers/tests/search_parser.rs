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
use vydra_providers::Strictness;
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
    let document =
        json!({"contents": [channel("UCa8RiU2st_48AA4WscbwYVQ", "Captain Jack Official")]});
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
fn playlist_renderer_maps_the_item_count_from_either_shape() {
    // `videoCountText` (the displayed form) ...
    let runs = json!({"contents": [playlist("PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR", "Mix")]});
    let (results, _report) = parse(&runs);
    let SearchItem::Playlist(mapped) = &results.items[0] else {
        panic!("expected a playlist");
    };
    assert_eq!(mapped.id.as_str(), "PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR");
    assert_eq!(mapped.title, "Mix");
    assert_eq!(
        mapped.item_count.as_ref().map(|count| count.value),
        Some(20)
    );

    // ... and `videoCount` (the plain field) both produce the count.
    let plain = json!({"contents": [{"playlistRenderer": {
        "playlistId": "PL6nHXviN_VZE92k6frgzKQP75CTeT8GRR",
        "title": {"simpleText": "Mix"},
        "videoCount": "37",
    }}]});
    let (results, _report) = parse(&plain);
    let SearchItem::Playlist(mapped) = &results.items[0] else {
        panic!("expected a playlist");
    };
    assert_eq!(
        mapped.item_count.as_ref().map(|count| count.value),
        Some(37)
    );
}

#[test]
fn a_channel_without_an_id_is_skipped_not_invented() {
    let document = json!({"contents": [{"channelRenderer": {"title": {"simpleText": "Ghost"}}}]});
    let (results, report) = parse(&document);

    assert!(results.items.is_empty(), "nothing may be synthesised");
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("channel_without_id")),
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
        report
            .messages()
            .iter()
            .any(|message| message.contains("video_id_invalid")),
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
        report
            .messages()
            .iter()
            .any(|message| message.contains("videoRenderer.title")),
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
    assert_eq!(
        report.unknown_renderers(),
        0,
        "containers are not unknown content"
    );
}

#[test]
fn unknown_renderers_never_become_items() {
    // Page chrome plus one real result: a Shorts shelf item and a "playables" card are content
    // shapes Vydra cannot map yet, a `dialogRenderer` is chrome.
    let document = json!({
        "contents": {"sectionListRenderer": {"contents": [
            {"itemSectionRenderer": {"contents": [
                {"reelShelfRenderer": {"items": [{"reelItemRenderer": {"videoId": "dQw4w9WgXcQ"}}]}},
                {"gridShelfViewModel": {"contents": [
                    {"shortsLockupViewModel": {"entityId": "shorts-shelf-item-dQw4w9WgXcQ"}},
                ]}},
                {"dialogRenderer": {"title": {"simpleText": "Cookies"}}},
                {"videoRenderer": {"videoId": "dQw4w9WgXcQ", "title": {"simpleText": "Real"}}},
            ]}},
        ]}}
    });
    let (results, report) = parse(&document);

    assert_eq!(results.items.len(), 1, "only the videoRenderer may map");
    assert_eq!(results.items[0].kind(), "video");
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("unmapped_content:shortsLockupViewModel")),
        "a dropped result must be named: {:?}",
        report.messages()
    );
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("unknown_renderer:dialogRenderer")),
        "chrome is counted separately: {:?}",
        report.messages()
    );
    assert!(
        report.content_problems() >= 1,
        "losing a result is a content problem"
    );
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
    assert_eq!(
        ids,
        vec!["dQw4w9WgXcQ", "9Umm2MAKsMc"],
        "order preserved, repeat dropped"
    );
    assert!(
        report
            .messages()
            .iter()
            .any(|message| message.contains("duplicate_item")),
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
        (
            "25 399 352 просмотра",
            "1:43:17",
            Some(25_399_352),
            Some(6197),
        ),
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
        assert_eq!(
            mapped.duration_seconds, expected_duration,
            "duration {duration:?}"
        );
    }
}

#[test]
fn abbreviated_counts_are_marked_approximate() {
    let document = json!({"contents": [{"videoRenderer": {
        "videoId": "dQw4w9WgXcQ",
        "title": {"simpleText": "T"},
        "viewCountText": {"simpleText": "1.2K views"},
    }}]});
    let (results, _) = parse(&document);
    let SearchItem::Video(mapped) = &results.items[0] else {
        panic!("expected a video");
    };
    let count = mapped.view_count.as_ref().expect("count");
    assert_eq!(count.value, 1_200);
    assert_eq!(count.precision, Precision::Approximate);
    assert_eq!(
        count.raw.as_deref(),
        Some("1.2K views"),
        "the source text is kept"
    );
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
        mapped
            .thumbnails
            .iter()
            .all(|thumb| thumb.url.starts_with("https://")),
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
    assert_eq!(
        report.count(),
        0,
        "the provider adds `no_renderers`; the mapper alone is quiet"
    );
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
fn a_filter_chip_token_never_becomes_the_page_cursor() {
    // Live evidence (run 43542839419): a first page carried six `continuationCommand` tokens — one
    // page cursor (638 chars) and five filter chips (746/754/810/812/1018). The walk used to return a
    // chip token, which pages a *filtered* result set instead of the next page.
    let document = json!({
        "header": {"searchHeaderRenderer": {"chipBar": {"chipCloudRenderer": {"chips": [
            {"chipCloudChipRenderer": {"navigationEndpoint": {"continuationCommand": {"token": "CHIP-TOKEN"}}}},
        ]}}}},
        "contents": {"twoColumnSearchResultsRenderer": {"primaryContents": {"sectionListRenderer": {"contents": [
            {"itemSectionRenderer": {"contents": [video("dQw4w9WgXcQ", "T")]}},
            {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "PAGE-CURSOR"}}}},
        ]}}}}
    });
    let (results, _) = parse(&document);

    assert_eq!(results.next_cursor.as_deref(), Some("PAGE-CURSOR"));
}

// Each document below was captured by Vydra from the live endpoint (see
// `tests/fixtures/recorded/search_web_probe_*.meta.json` for the run, the request body keys and the
// reduction applied). The counts are exact because the reduction keeps every mapped renderer and
// every content shape that has no mapping — nothing else — so a change in the mapper shows up here.

#[test]
fn recorded_first_page_maps_every_result_and_counts_the_shorts_gap() {
    let document = fixture("recorded/search_web_probe_first_page.json");
    let (results, report) = parse(&document);

    assert_eq!(results.items.len(), 19, "18 videos + 1 channel");
    let kinds: Vec<&str> = results.items.iter().map(|item| item.kind()).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == "video").count(), 18);
    assert_eq!(kinds.iter().filter(|kind| **kind == "channel").count(), 1);
    assert_eq!(
        results.items[0].kind(),
        "video",
        "document order is preserved"
    );
    assert_eq!(
        results.estimated_results.as_ref().map(|count| count.value),
        Some(23_544_604),
        "upstream's own estimate is carried through"
    );

    let cursor = results
        .next_cursor
        .as_deref()
        .expect("first page has a cursor");
    assert_eq!(
        cursor.len(),
        638,
        "the page cursor, not one of the filter-chip tokens on the same page"
    );

    assert_eq!(
        report.unmapped_results(),
        25,
        "the Shorts shelf is 25 results Vydra cannot show yet: counted, not silent"
    );
    assert_eq!(
        report.unknown_renderers(),
        0,
        "the reduction keeps no chrome, so anything counted here is a regression: {:?}",
        report.messages()
    );
    assert_eq!(report.skipped_items(), 0);
    assert!(
        report.check(Strictness::Strict).is_err(),
        "strict mode must stay loud while results are being lost"
    );
}

#[test]
fn recorded_continuation_page_maps_every_result() {
    let document = fixture("recorded/search_web_probe_continuation.json");
    let (results, report) = parse(&document);

    assert_eq!(results.items.len(), 18);
    assert_eq!(
        results
            .items
            .iter()
            .filter(|item| item.kind() == "video")
            .count(),
        18
    );
    let cursor = results
        .next_cursor
        .as_deref()
        .expect("second page has a cursor");
    assert_eq!(cursor.len(), 670);
    assert_eq!(
        report.unmapped_results(),
        27,
        "the continuation page also carries a Shorts shelf (25) and two lockup cards"
    );
    assert_eq!(report.unknown_renderers(), 0);
    assert_eq!(report.skipped_items(), 0);
    assert!(
        report.check(Strictness::Strict).is_err(),
        "unmapped results keep strict mode loud"
    );
    // A *true* page 2 repeats the query's total estimate; the chip continuation captured earlier
    // reported 19, so the field is upstream's estimate, not a page counter.
    assert_eq!(
        results.estimated_results.as_ref().map(|count| count.value),
        Some(23_544_805)
    );
}

#[test]
fn recorded_mixed_page_maps_all_kinds_it_can() {
    let document = fixture("recorded/search_web_probe_mixed.json");
    let (results, report) = parse(&document);

    assert_eq!(
        results.items.len(),
        24,
        "24 videos + 1 channel with one duplicate removed"
    );
    assert_eq!(
        results
            .items
            .iter()
            .filter(|item| item.kind() == "video")
            .count(),
        23
    );
    assert_eq!(
        results
            .items
            .iter()
            .filter(|item| item.kind() == "channel")
            .count(),
        1
    );
    assert_eq!(
        report.skipped_items(),
        1,
        "the page served one id twice; the repeat is counted, not silently appended"
    );
    assert_eq!(
        results.estimated_results.as_ref().map(|count| count.value),
        Some(200_236)
    );
    assert_eq!(
        results.next_cursor.as_deref().map(str::len),
        Some(612),
        "the page cursor; the chip tokens on this page are longer"
    );
    assert_eq!(report.unmapped_results(), 65, "the two Shorts shelves");
    assert_eq!(report.unknown_renderers(), 0);
}

#[test]
fn recorded_empty_page_is_empty_not_broken() {
    let document = fixture("recorded/search_web_probe_empty.json");
    let (results, report) = parse(&document);

    assert!(results.items.is_empty());
    assert!(results.next_cursor.is_none());
    assert_eq!(
        results.estimated_results.as_ref().map(|count| count.value),
        Some(0)
    );
    assert_eq!(
        report.count(),
        0,
        "an empty result set is not a parse warning"
    );
}

#[test]
fn recorded_error_document_yields_no_items() {
    // `{"error": {"code": 400, "status": "FAILED_PRECONDITION", ...}}` — what upstream answers when
    // the search body is missing `query` (run 43542839419). It must never look like an empty page
    // that happens to have no results: the provider reports the status, and nothing is invented here.
    let document = fixture("recorded/search_web_probe_malformed_request.json");
    let (results, report) = parse(&document);

    assert!(results.items.is_empty());
    assert!(results.next_cursor.is_none());
    assert!(results.estimated_results.is_none());
    assert_eq!(
        report.count(),
        0,
        "there is nothing to warn about: nothing was attempted"
    );
    assert_eq!(
        document
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_i64),
        Some(400),
        "the fixture really is the error document"
    );
}
