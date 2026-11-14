//! Phase 6 upstream **browse evidence probe** (manual, network-using).
//!
//! Not a test. It performs a small, fixed set of real browse requests to `www.youtube.com` through the
//! production [`ReqwestTransport`], the production session bootstrap and the production request types —
//! the same allowlist, deadline and size cap Vydra uses at runtime — and writes what came back, so that
//! "recorded fixture" can mean what the corpus says it means.
//!
//! The fixed request set (no enumeration, no id guessing; every resource is public):
//!
//! | # | Request | Why |
//! | --- | --- | --- |
//! | 1 | `browse` `{browseId: UC…}` | channel document shape |
//! | 2 | `browse` `{browseId: UC…, params: <relayed videos tab>}` | proves the tab `params` relay is what the page itself carries |
//! | 3 | `browse` `{continuation}` from 2 | proves the page token is a genuine continuation |
//! | 4 | `browse` `{browseId: VL…}` for the channel's uploads playlist | playlist document shape |
//! | 5 | `browse` `{continuation}` from 4 | entry-page shape and position continuity |
//! | 6 | `browse` `{browseId: @handle}` | whether a handle is a valid browse id |
//!
//! Nothing here bypasses a bot check, a sign-in gate or a consent flow, and a refused request is recorded
//! as a refusal — no body is written and no document is fabricated to fill the gap.
//!
//! Outputs in `VYDRA_PROBE_OUT` (default `probe-out/`): one redacted JSON capture per successful request,
//! plus `summary.json` (one record per capture, with byte count and SHA-256). The channel page is also
//! printed as base64 in bounded chunks, so a genuine capture can be recovered from a CI log when the
//! artifact is not reachable.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use vydra_common::{ChannelId, ChannelRef, PlaylistId};
use vydra_providers::innertube::browse_map::{
    map_channel, map_channel_videos, map_playlist, map_playlist_videos, videos_tab_params,
};
use vydra_providers::innertube::{
    BrowseRequest, ClientContext, ClientKind, ClientProfile, ContinuationRequest, InnertubeClient,
    InnertubeProviderConfig,
};
use vydra_providers::parse::find_page_cursor;
use vydra_providers::{ProviderContext, ReqwestTransport, Strictness};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

/// Session-scoped, advertising and tracking material: no evidentiary value, some of it identity-bearing.
const DROP_KEYS: &[&str] = &[
    "responseContext",
    "frameworkUpdates",
    "trackingParams",
    "clickTrackingParams",
    "serviceTrackingParams",
    "loggingContext",
    "adPlacements",
    "playerAds",
    "visitorData",
    "apiKey",
    "consentCookie",
];

const SECRET_MARKERS: &[&str] = &["AIza", "sigh=", "&sig=", "?sig="];

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn context() -> ProviderContext {
    ProviderContext {
        deadline: Instant::now() + Duration::from_secs(120),
        request_id: "phase-6-probe".to_owned(),
        hl: "en".to_owned(),
        gl: "US".to_owned(),
        strictness: Strictness::Lenient,
    }
}

fn strip_query(text: &str) -> String {
    match text.split_once('?') {
        Some((head, _)) => head.to_owned(),
        None => text.to_owned(),
    }
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if DROP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                out.insert(key, redact(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        // Thumbnail and banner URLs carry `?sqp=` and signature-ish parameters; the path is the evidence.
        Value::String(text) if text.starts_with("http") => Value::String(strip_query(&text)),
        other => other,
    }
}

fn redacted_ok(redacted: &Value) -> bool {
    let text = redacted.to_string();
    !SECRET_MARKERS.iter().any(|marker| text.contains(marker))
}

/// FNV-1a 64-bit, printed as hex.
///
/// Not cryptographic: it exists so a capture in the CI log can be matched to the artifact bytes at a
/// glance. Recorded fixtures committed to the corpus carry a SHA-256 computed by the archiver
/// (`scripts/fixtures/archive_probe.py`), which is what `docs/research/CHANNEL_PLAYLIST_FIXTURE_PROVENANCE.md`
/// records.
fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn base64_bytes(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Write one redacted capture and record what was written.
fn write_capture(
    out_dir: &Path,
    name: &str,
    payload: &Value,
    summary: &mut Vec<Value>,
) -> ProbeResult<()> {
    let redacted = redact(payload.clone());
    if !redacted_ok(&redacted) {
        eprintln!("PROBE {name} withheld: redaction self-check failed");
        summary.push(json!({"capture": name, "written": false, "reason": "redaction_self_check"}));
        return Ok(());
    }
    let text = serde_json::to_string_pretty(&redacted)?;
    let path = out_dir.join(format!("{name}.json"));
    fs::write(&path, text.as_bytes())?;

    let mut top_level_keys: Vec<String> = redacted
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    top_level_keys.sort();
    let digest = fnv1a64_hex(text.as_bytes());
    summary.push(json!({
        "capture": name,
        "file": path.display().to_string(),
        "bytes": text.len(),
        "checksum_fnv1a64": digest,
        "top_level_keys": top_level_keys,
    }));
    println!("PROBE capture={name} bytes={} fnv1a64={digest}", text.len());
    Ok(())
}

/// Print a redacted body as base64 in bounded chunks, so it survives in a CI log.
fn dump_base64(name: &str, payload: &Value) {
    let text = match serde_json::to_string(&redact(payload.clone())) {
        Ok(text) => text,
        Err(_) => return,
    };
    if text.len() > 400_000 {
        println!("PROBE b64-skip={name} reason=too_large bytes={}", text.len());
        return;
    }
    let encoded = base64_bytes(text.as_bytes());
    println!(
        "PROBE b64-begin={name} bytes={} b64_len={}",
        text.len(),
        encoded.len()
    );
    for (index, chunk) in encoded.as_bytes().chunks(1000).enumerate() {
        println!("PROBE b64 {index} {}", String::from_utf8_lossy(chunk));
    }
    println!("PROBE b64-end={name}");
}

fn synthetic_provenance(fetched_at: OffsetDateTime) -> vydra_domain::Provenance {
    vydra_domain::Provenance {
        provider: "innertube-web".to_owned(),
        endpoint: "browse".to_owned(),
        client: Some("web".to_owned()),
        fetched_at,
        cached: false,
        parse_warnings: 0,
        sources: vec![],
    }
}

fn body_of<T: Serialize>(request: &T) -> ProbeResult<Value> {
    Ok(serde_json::to_value(request)?)
}

async fn browse(
    client: &InnertubeClient,
    request: BrowseRequest,
    ctx: &ProviderContext,
) -> Result<Value, String> {
    let body = body_of(&request).map_err(|error| error.to_string())?;
    match client.post_json("browse", body, ctx).await {
        Ok((payload, _)) => Ok(payload),
        Err(error) => Err(format!("{} ({})", error, error.code())),
    }
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    // A long-standing public channel ("Veritasium") and its uploads playlist (`UU` + id without `UC`).
    let channel_id_raw = env_or("VYDRA_PROBE_CHANNEL_ID", "UCHnyfMqiRRG1u-2MsSQLbXA");
    let handle_raw = env_or("VYDRA_PROBE_CHANNEL_HANDLE", "@veritasium");
    let out_dir = PathBuf::from(env_or("VYDRA_PROBE_OUT", "probe-out"));
    fs::create_dir_all(&out_dir)?;

    let channel_id = ChannelId::parse(&channel_id_raw)?;
    let transport = Arc::new(ReqwestTransport::new()?);
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    let client = InnertubeClient::new(config, transport);
    let ctx = context();

    let mut summary: Vec<Value> = Vec::new();
    let mut captures: Vec<(String, Value)> = Vec::new();

    fn record(
        name: &str,
        outcome: Result<Value, String>,
        captures: &mut Vec<(String, Value)>,
    ) -> Value {
        match outcome {
            Ok(payload) => {
                captures.push((name.to_owned(), payload.clone()));
                json!({"capture": name, "outcome": "ok", "bytes": payload.to_string().len()})
            }
            Err(message) => {
                println!(
                    "PROBE {name}=error={}",
                    message.chars().take(200).collect::<String>()
                );
                json!({"capture": name, "outcome": "error", "message": message.chars().take(200).collect::<String>()})
            }
        }
    }

    // 1 — channel page by canonical id.
    let session = client.session(&ctx).await;
    let channel_context = client.context(&session, &ctx);
    let channel_page = browse(
        &client,
        BrowseRequest::channel(channel_context.clone(), &ChannelRef::Id(channel_id.clone())),
        &ctx,
    )
    .await;
    summary.push(record("channel_page", channel_page, &mut captures));

    // 6 — channel page by handle (recorded for completeness; a failure here is a finding, not a bug).
    let handle_ref = ChannelRef::parse(&handle_raw)?;
    let handle_page = browse(
        &client,
        BrowseRequest::channel(channel_context.clone(), &handle_ref),
        &ctx,
    )
    .await;
    summary.push(record("channel_page_by_handle", handle_page, &mut captures));

    // 2 — videos tab, addressed with the params the channel page itself carried.
    let mut videos_page: Option<Value> = None;
    if let Some((_, page)) = captures
        .iter()
        .find(|(name, _)| name == "channel_page")
        .cloned()
    {
        match videos_tab_params(&page) {
            Some(params) => {
                println!("PROBE videos_tab_params_len={}", params.len());
                let request =
                    BrowseRequest::channel_tab(channel_context.clone(), &channel_id, params);
                match request {
                    Ok(request) => {
                        let outcome = browse(&client, request, &ctx).await;
                        if let Ok(payload) = &outcome {
                            videos_page = Some(payload.clone());
                        }
                        summary.push(record("channel_videos_page1", outcome, &mut captures));
                    }
                    Err(error) => {
                        summary.push(json!({"capture": "channel_videos_page1", "outcome": "invalid_params", "message": error.to_string()}));
                    }
                }
            }
            None => {
                // A finding in itself: no videos tab means the relay has nothing to carry.
                println!("PROBE videos_tab_params=none");
                summary.push(json!({"capture": "channel_videos_page1", "outcome": "no_videos_tab"}));
            }
        }
    }

    // 3 — page 2, with the token page 1 carried.
    let videos_token = videos_page.as_ref().and_then(find_page_cursor);
    match videos_token {
        Some(token) => {
            println!("PROBE channel_videos_token_len={}", token.len());
            let request = ContinuationRequest::new(channel_context.clone(), token);
            let outcome = match serde_json::to_value(&request) {
                Ok(body) => match client.post_json("browse", body, &ctx).await {
                    Ok((payload, _)) => Ok(payload),
                    Err(error) => Err(format!("{} ({})", error, error.code())),
                },
                Err(error) => Err(error.to_string()),
            };
            summary.push(record("channel_videos_page2", outcome, &mut captures));
        }
        None => {
            println!("PROBE channel_videos_token=none");
            summary.push(json!({"capture": "channel_videos_page2", "outcome": "no_page_cursor"}));
        }
    }

    // 4 — the channel's uploads playlist (`UU` + id without `UC`).
    let uploads = format!("UU{}", &channel_id_raw[2..]);
    let uploads_id = PlaylistId::parse(&uploads)?;
    let playlist_page = browse(
        &client,
        BrowseRequest::playlist(channel_context.clone(), &uploads_id),
        &ctx,
    )
    .await;
    let playlist_payload = playlist_page.as_ref().ok().cloned();
    summary.push(record("playlist_page", playlist_page, &mut captures));

    // 5 — one entry continuation page of that playlist.
    match playlist_payload.as_ref().and_then(find_page_cursor) {
        Some(token) => {
            println!("PROBE playlist_token_len={}", token.len());
            let request = ContinuationRequest::new(channel_context.clone(), token);
            let outcome = match serde_json::to_value(&request) {
                Ok(body) => match client.post_json("browse", body, &ctx).await {
                    Ok((payload, _)) => Ok(payload),
                    Err(error) => Err(format!("{} ({})", error, error.code())),
                },
                Err(error) => Err(error.to_string()),
            };
            summary.push(record("playlist_page2", outcome, &mut captures));
        }
        None => {
            println!("PROBE playlist_token=none");
            summary.push(json!({"capture": "playlist_page2", "outcome": "no_page_cursor"}));
        }
    }

    // Map the genuine captures with the production mappers, so the summary states what Vydra would serve.
    let fetched_at = OffsetDateTime::now_utc();
    if let Some((_, page)) = captures
        .iter()
        .find(|(name, _)| name == "channel_page")
        .cloned()
    {
        let mut parse_report = vydra_providers::parse::ParseReport::new();
        match map_channel(
            &page,
            &ChannelRef::Id(channel_id.clone()),
            &mut parse_report,
            synthetic_provenance(fetched_at),
        ) {
            Ok(document) => println!(
                "PROBE mapped_channel id={} title_bytes={} tabs={:?} subscribers={:?} videos={:?} warnings={} sections={:?}",
                document.summary.id.as_str(),
                document.summary.title.len(),
                document
                    .tabs
                    .iter()
                    .map(|tab| tab.slug.clone().unwrap_or_else(|| "?".to_owned()))
                    .collect::<Vec<_>>(),
                document.summary.subscriber_count.as_ref().map(|count| count.value),
                document.summary.video_count.as_ref().map(|count| count.value),
                parse_report.count(),
                document
                    .evidence
                    .sections
                    .iter()
                    .map(|section| section.as_str())
                    .collect::<Vec<_>>(),
            ),
            Err(error) => println!("PROBE mapped_channel=error={error}"),
        }
    }
    if let Some(page) = videos_page.as_ref() {
        let mut parse_report = vydra_providers::parse::ParseReport::new();
        let list =
            map_channel_videos(page, 30, &mut parse_report, synthetic_provenance(fetched_at));
        println!(
            "PROBE mapped_channel_videos items={} duplicates={} next={} warnings={}",
            list.items.len(),
            list.duplicates_removed,
            list.next_cursor.is_some(),
            parse_report.count()
        );
    }
    if let Some(page) = playlist_payload.as_ref() {
        let mut parse_report = vydra_providers::parse::ParseReport::new();
        match map_playlist(
            page,
            &uploads_id,
            30,
            &mut parse_report,
            synthetic_provenance(fetched_at),
        ) {
            Ok(document) => {
                let positions: Vec<Option<u32>> =
                    document.items.iter().take(5).map(|item| item.position).collect();
                let unavailable = document
                    .items
                    .iter()
                    .filter(|item| !item.availability.is_available())
                    .count();
                println!(
                    "PROBE mapped_playlist items={} positions={positions:?} unavailable={unavailable}",
                    document.items.len(),
                );
                println!(
                    "PROBE mapped_playlist repeated={:?} item_count={:?} warnings={}",
                    document.repeated_entries,
                    document.summary.item_count.as_ref().map(|count| count.value),
                    parse_report.count(),
                );
            }
            Err(error) => println!("PROBE mapped_playlist=error={error}"),
        }
    }
    if let Some((_, page)) = captures.iter().find(|(name, _)| name == "playlist_page2") {
        let mut parse_report = vydra_providers::parse::ParseReport::new();
        let list =
            map_playlist_videos(page, 30, &mut parse_report, synthetic_provenance(fetched_at));
        println!(
            "PROBE mapped_playlist_page2 items={} first_position={:?} warnings={}",
            list.items.len(),
            list.items.first().and_then(|item| item.position),
            parse_report.count()
        );
    }

    // Persist the captures (redacted) and the summary.
    for (name, payload) in &captures {
        write_capture(&out_dir, name, payload, &mut summary)?;
        if name == "channel_page" {
            dump_base64(name, payload);
        }
    }

    let summary_doc = json!({
        "probe": "phase-6-browse",
        "captured_at": fetched_at.format(&Rfc3339)?,
        "endpoint": "browse",
        "client": "web",
        "class": "recorded_vydra_probe",
        "channel_id": channel_id_raw,
        "handle": handle_raw,
        "captures": summary,
        "notes": [
            "bodies are redacted: tracking/identity keys dropped and URL query strings removed",
            "a refused request is recorded as a refusal; no body is written for it",
            "the channel page body is also printed as base64 in the CI log",
        ],
    });
    fs::write(
        out_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary_doc)?,
    )?;
    println!("PROBE summary={}", serde_json::to_string(&summary_doc)?);
    Ok(())
}
