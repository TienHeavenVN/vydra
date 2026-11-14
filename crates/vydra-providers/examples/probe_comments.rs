//! Phase 7 upstream **comment evidence probe** (manual, network-using).
//!
//! Not a test. It performs a small, fixed set of real `next` requests to `www.youtube.com` through the
//! production [`ReqwestTransport`], the production session bootstrap and the production request types, and
//! writes what came back, so that "recorded fixture" can mean what the corpus says it means.
//!
//! The fixed request set (no enumeration, no id guessing, no sign-in; every resource is public):
//!
//! | # | Request | Why |
//! | --- | --- | --- |
//! | 1 | `next` `{videoId}` | does the watch page carry `itemSectionRenderer.targetId == "comments-section"`? |
//! | 2 | `next` `{continuation}` from 1 | comment page shape: renderer kinds, entity batch update, sort menu |
//! | 3 | `next` `{continuation}` for the **other** sort order, from 2's own menu | proves an order token is a genuine request contract |
//! | 4 | `next` `{continuation}` for the first thread's replies, from 2 | reply page shape |
//! | 5 | `next` `{videoId}` for an id expected not to resolve | what "no comment section" looks like |
//!
//! Nothing here posts, edits, deletes or likes anything: every request is a read. A refused request is
//! recorded as a refusal — no body is written and no document is fabricated to fill the gap.
//!
//! Outputs in `VYDRA_PROBE_OUT` (default `probe-out/`): one redacted JSON capture per successful request,
//! plus `summary.json`. The comment page is also printed as base64 in bounded chunks so a genuine capture
//! can be recovered from a CI log when the artifact is not reachable.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use vydra_common::{CommentId, VideoId};
use vydra_domain::CommentSort;
use vydra_providers::innertube::comments::{
    comments_section_token, map_comments_page, map_replies_page, renderer_keys, replies_token_for,
    sort_options, sort_token,
};
use vydra_providers::innertube::{
    ClientContext, ClientKind, ClientProfile, InnertubeClient, InnertubeProviderConfig, NextRequest,
};
use vydra_providers::parse::ParseReport;
use vydra_providers::{ProviderContext, ReqwestTransport, Strictness};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

/// Session-scoped, advertising and tracking material: no evidentiary value, some of it identity-bearing.
const DROP_KEYS: &[&str] = &[
    "responseContext",
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

/// Comment text is user-generated content: the probe keeps the *shape* of a body, never its prose.
const REDACT_TEXT_KEYS: &[&str] = &["content", "contentText", "sourceText", "displayName"];

const SECRET_MARKERS: &[&str] = &["AIza", "sigh=", "&sig=", "?sig="];

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

fn context() -> ProviderContext {
    ProviderContext {
        deadline: Instant::now() + Duration::from_secs(120),
        request_id: "phase-7-probe".to_owned(),
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

/// Redact identity-bearing keys, URL query strings, and the prose of user-generated content.
///
/// Comment text is replaced by `<redacted len=N>`: the length is evidence about the shape of a comment and
/// the words are somebody's, so the words go. Author display names are treated the same way, because a
/// comment section is a list of private individuals' names.
fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if DROP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if REDACT_TEXT_KEYS.contains(&key.as_str())
                    && let Some(text) = child.as_str()
                {
                    if !text.is_empty() {
                        out.insert(key, Value::String(format!("<redacted len={}>", text.len())));
                        continue;
                    }
                }
                out.insert(key, redact(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        Value::String(text) if text.starts_with("http") => Value::String(strip_query(&text)),
        other => other,
    }
}

fn redacted_ok(redacted: &Value) -> bool {
    let text = redacted.to_string();
    !SECRET_MARKERS.iter().any(|marker| text.contains(marker))
}

/// FNV-1a 64-bit, printed as hex: enough to match a capture in the log to the artifact bytes.
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

    let digest = fnv1a64_hex(text.as_bytes());
    summary.push(json!({
        "capture": name,
        "file": path.display().to_string(),
        "bytes": text.len(),
        "checksum_fnv1a64": digest,
    }));
    println!("PROBE capture={name} bytes={} fnv1a64={digest}", text.len());
    Ok(())
}

fn dump_base64(name: &str, payload: &Value) {
    let text = match serde_json::to_string(&redact(payload.clone())) {
        Ok(text) => text,
        Err(_) => return,
    };
    if text.len() > 400_000 {
        println!(
            "PROBE b64-skip={name} reason=too_large bytes={}",
            text.len()
        );
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

/// Which renderer/view-model keys the page carries and how many of each.
fn shape_counts(payload: &Value) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut stack = vec![payload];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 400_000 {
            break;
        }
        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    let is_shape = key.ends_with("Renderer")
                        || key.ends_with("ViewModel")
                        || key.ends_with("Command")
                        || key.ends_with("Action");
                    if is_shape && !value.is_null() {
                        *counts.entry(key.clone()).or_insert(0) += 1;
                    }
                    if value.is_object() || value.is_array() {
                        stack.push(value);
                    }
                }
            }
            Value::Array(items) => {
                for item in items.iter().rev() {
                    if item.is_object() || item.is_array() {
                        stack.push(item);
                    }
                }
            }
            _ => {}
        }
    }
    counts
}

fn provenance(endpoint: &str, fetched_at: OffsetDateTime) -> vydra_domain::Provenance {
    vydra_domain::Provenance {
        provider: "innertube-web".to_owned(),
        endpoint: endpoint.to_owned(),
        client: Some("web".to_owned()),
        fetched_at,
        cached: false,
        parse_warnings: 0,
        sources: vec![],
    }
}

async fn next(
    client: &InnertubeClient,
    request: NextRequest,
    ctx: &ProviderContext,
) -> Result<Value, String> {
    let body = serde_json::to_value(&request).map_err(|error| error.to_string())?;
    match client.post_json("next", body, ctx).await {
        Ok((payload, _)) => Ok(payload),
        Err(error) => Err(format!("{} ({})", error, error.code())),
    }
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let video_raw = env_or("VYDRA_PROBE_COMMENTS_VIDEO", "dQw4w9WgXcQ");
    let missing_raw = env_or("VYDRA_PROBE_COMMENTS_MISSING_VIDEO", "AAAAAAAAAAA");
    let out_dir = PathBuf::from(env_or("VYDRA_PROBE_OUT", "probe-out"));
    fs::create_dir_all(&out_dir)?;

    let video_id = VideoId::parse(&video_raw)?;
    let transport = Arc::new(ReqwestTransport::new()?);
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        ..Default::default()
    });
    let client = InnertubeClient::new(config, transport);
    let ctx = context();

    let mut summary: Vec<Value> = Vec::new();
    let mut captures: Vec<(String, Value)> = Vec::new();
    let fetched_at = OffsetDateTime::now_utc();

    // 1 — the watch page, which is where the comment section's token comes from.
    let session = client.session(&ctx).await;
    let watch_context: ClientContext = client.context(&session, &ctx);
    let watch = next(
        &client,
        NextRequest::comments_section(watch_context.clone(), &video_id),
        &ctx,
    )
    .await;
    let section_token = match &watch {
        Ok(payload) => {
            captures.push(("watch_page".to_owned(), payload.clone()));
            match comments_section_token(payload) {
                Ok(Some(token)) => {
                    println!("PROBE comments_section_token=present len={}", token.len());
                    summary.push(json!({
                        "capture": "watch_page",
                        "outcome": "ok",
                        "section_token": "present",
                        "section_token_len": token.len(),
                    }));
                    Some(token)
                }
                Ok(None) => {
                    println!("PROBE comments_section_token=absent");
                    summary.push(json!({
                        "capture": "watch_page",
                        "outcome": "ok",
                        "section_token": "absent",
                        "interpretation": "comments disabled upstream (section absent, not empty)",
                    }));
                    None
                }
                Err(error) => {
                    println!("PROBE comments_section_token=parse_error={error}");
                    summary.push(json!({
                        "capture": "watch_page",
                        "outcome": "parse_error",
                        "message": error.to_string(),
                    }));
                    None
                }
            }
        }
        Err(message) => {
            println!("PROBE watch_page=error={message}");
            summary.push(json!({"capture": "watch_page", "outcome": "error", "message": message}));
            None
        }
    };

    // 2 — comment page 1, addressed by the token the watch page carried.
    if let Some(token) = section_token {
        match next(
            &client,
            NextRequest::continuation(watch_context.clone(), token)?,
            &ctx,
        )
        .await
        {
            Ok(payload) => {
                captures.push(("comments_page1".to_owned(), payload.clone()));
                let counts = shape_counts(&payload);
                println!(
                    "PROBE struct_comments_page1 {}",
                    counts
                        .iter()
                        .map(|(key, count)| format!("{key}x{count}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                println!(
                    "PROBE row_keys {:?}",
                    renderer_keys(&payload)
                        .into_iter()
                        .map(|(key, count)| format!("{key}x{count}"))
                        .collect::<Vec<_>>()
                );

                let options = sort_options(&payload);
                println!(
                    "PROBE sort_menu options={:?}",
                    options
                        .iter()
                        .map(|o| (o.title.clone(), o.selected, o.token.len()))
                        .collect::<Vec<_>>()
                );

                let mut report = ParseReport::new();
                let list = map_comments_page(
                    &payload,
                    CommentSort::Top,
                    20,
                    &mut report,
                    provenance("next", fetched_at),
                );
                let pinned = list.items.iter().filter(|t| t.comment.is_pinned).count();
                let hearted = list.items.iter().filter(|t| t.comment.is_hearted).count();
                let with_replies = list
                    .items
                    .iter()
                    .filter(|t| !t.loaded_replies.is_empty())
                    .count();
                let more_replies = list.items.iter().filter(|t| t.has_more_replies).count();
                let deleted = list
                    .items
                    .iter()
                    .filter(|t| !t.comment.availability.is_available())
                    .count();
                println!(
                    "PROBE mapped_comments items={} duplicates={} next={} pinned={pinned} hearted={hearted} with_replies={with_replies} more_replies={more_replies} deleted={deleted} warnings={:?}",
                    list.items.len(),
                    list.duplicates,
                    list.next_token.is_some(),
                    report.messages()
                );
                summary.push(json!({
                    "capture": "comments_page1",
                    "outcome": "ok",
                    "bytes": payload.to_string().len(),
                    "shape_counts": counts,
                    "row_keys": renderer_keys(&payload),
                    "sort_menu": options
                        .iter()
                        .map(|o| json!({"title": o.title, "selected": o.selected, "token_len": o.token.len()}))
                        .collect::<Vec<_>>(),
                    "mapped": {
                        "items": list.items.len(),
                        "duplicates": list.duplicates,
                        "next": list.next_token.is_some(),
                        "pinned": pinned,
                        "hearted": hearted,
                        "with_loaded_replies": with_replies,
                        "with_more_replies": more_replies,
                        "unavailable_rows": deleted,
                    },
                    "parse_warnings": report.messages(),
                }));

                // 3 — the other order, addressed with the token the page's own menu carried.
                if let Ok(other_token) = sort_token(&options, CommentSort::Newest) {
                    let same_as_first = list
                        .items
                        .first()
                        .map(|thread| thread.comment.id.as_str().to_owned());
                    match next(
                        &client,
                        NextRequest::continuation(watch_context.clone(), other_token)?,
                        &ctx,
                    )
                    .await
                    {
                        Ok(payload) => {
                            captures.push(("comments_page1_newest".to_owned(), payload.clone()));
                            let mut other_report = ParseReport::new();
                            let other = map_comments_page(
                                &payload,
                                CommentSort::Newest,
                                20,
                                &mut other_report,
                                provenance("next", fetched_at),
                            );
                            let other_first =
                                other.items.first().map(|t| t.comment.id.as_str().to_owned());
                            println!(
                                "PROBE mapped_comments_newest items={} next={} first_same_as_top={}",
                                other.items.len(),
                                other.next_token.is_some(),
                                other_first == same_as_first
                            );
                            summary.push(json!({
                                "capture": "comments_page1_newest",
                                "outcome": "ok",
                                "items": other.items.len(),
                                "next": other.next_token.is_some(),
                                "first_row_same_as_top": other_first == same_as_first,
                                "parse_warnings": other_report.messages(),
                            }));
                        }
                        Err(message) => {
                            println!("PROBE comments_page1_newest=error={message}");
                            summary.push(json!({
                                "capture": "comments_page1_newest",
                                "outcome": "error",
                                "message": message,
                            }));
                        }
                    }
                } else {
                    println!("PROBE sort_menu=no_newest_option");
                    summary.push(json!({
                        "capture": "comments_page1_newest",
                        "outcome": "unavailable",
                        "message": "the sort menu did not offer a second order",
                    }));
                }

                // 4 — replies of the first thread that offered a reply continuation.
                let parent = list
                    .items
                    .iter()
                    .find(|thread| replies_token_for(&payload, &thread.comment.id).is_some())
                    .map(|thread| thread.comment.id.clone());
                match parent {
                    Some(parent) => {
                        probe_replies(
                            &client,
                            &watch_context,
                            &ctx,
                            &payload,
                            &parent,
                            fetched_at,
                            &mut captures,
                            &mut summary,
                        )
                        .await?;
                    }
                    None => {
                        println!("PROBE replies=no_thread_with_reply_token");
                        summary.push(json!({
                            "capture": "replies_page1",
                            "outcome": "unavailable",
                            "message": "no thread on page 1 rendered a reply continuation",
                        }));
                    }
                }

            }
            Err(message) => {
                println!("PROBE comments_page1=error={message}");
                summary.push(json!({
                    "capture": "comments_page1",
                    "outcome": "error",
                    "message": message,
                }));
            }
        }
    }

    // 5 — a video id expected not to resolve: what a missing comment section looks like.
    if let Ok(missing_id) = VideoId::parse(&missing_raw) {
        let missing = next(
            &client,
            NextRequest::comments_section(watch_context.clone(), &missing_id),
            &ctx,
        )
        .await;
        match missing {
            Ok(payload) => {
                let token = comments_section_token(&payload).ok().flatten();
                println!(
                    "PROBE missing_video comments_section_token={}",
                    if token.is_some() { "present" } else { "absent" }
                );
                summary.push(json!({
                    "capture": "missing_video_watch",
                    "outcome": "ok",
                    "section_token": if token.is_some() { "present" } else { "absent" },
                    "top_level_keys": payload
                        .as_object()
                        .map(|map| {
                            let mut keys: Vec<String> = map.keys().cloned().collect();
                            keys.sort();
                            keys
                        })
                        .unwrap_or_default(),
                }));
            }
            Err(message) => {
                println!("PROBE missing_video=error={message}");
                summary.push(json!({
                    "capture": "missing_video_watch",
                    "outcome": "error",
                    "message": message,
                }));
            }
        }
    }

    for (name, payload) in &captures {
        write_capture(&out_dir, name, payload, &mut summary)?;
        if name == "comments_page1" {
            dump_base64(name, payload);
        }
    }

    let summary_doc = json!({
        "probe": "phase-7-comments",
        "captured_at": fetched_at.format(&Rfc3339)?,
        "endpoint": "next",
        "client": "web",
        "class": "recorded_vydra_probe",
        "video_id": video_raw,
        "captures": summary,
        "notes": [
            "bodies are redacted: tracking/identity keys dropped, URL query strings removed, and comment text and author names replaced by their length",
            "a refused request is recorded as a refusal; no body is written for it",
            "the first comment page body is also printed as base64 in the CI log",
            "no comment was posted, edited, deleted or liked: every request is a read",
        ],
    });
    fs::write(
        out_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary_doc)?,
    )?;
    println!("PROBE summary={}", serde_json::to_string(&summary_doc)?);
    Ok(())
}

/// Fetch and map one reply page, then report what it contained.
#[allow(clippy::too_many_arguments)]
async fn probe_replies(
    client: &InnertubeClient,
    watch_context: &ClientContext,
    ctx: &ProviderContext,
    page1: &Value,
    parent: &CommentId,
    fetched_at: OffsetDateTime,
    captures: &mut Vec<(String, Value)>,
    summary: &mut Vec<Value>,
) -> ProbeResult<()> {
    let Some(token) = replies_token_for(page1, parent) else {
        return Ok(());
    };
    println!(
        "PROBE replies_token=present parent_len={} token_len={}",
        parent.as_str().len(),
        token.len()
    );
    match next(
        client,
        NextRequest::continuation(watch_context.clone(), token)?,
        ctx,
    )
    .await
    {
        Ok(payload) => {
            captures.push(("replies_page1".to_owned(), payload.clone()));
            let mut report = ParseReport::new();
            let page = map_replies_page(
                &payload,
                parent,
                20,
                &mut report,
                provenance("next", fetched_at),
            );
            let wrong_parent = page
                .items
                .iter()
                .filter(|reply| reply.parent_id.as_ref() != Some(parent))
                .count();
            println!(
                "PROBE mapped_replies items={} duplicates={} next={} wrong_parent={wrong_parent} warnings={:?}",
                page.items.len(),
                page.duplicates,
                page.next_token.is_some(),
                report.messages()
            );
            summary.push(json!({
                "capture": "replies_page1",
                "outcome": "ok",
                "parent_token_len": parent.as_str().len(),
                "items": page.items.len(),
                "duplicates": page.duplicates,
                "next": page.next_token.is_some(),
                "rows_with_another_parent": wrong_parent,
                "row_keys": renderer_keys(&payload),
                "parse_warnings": report.messages(),
            }));
        }
        Err(message) => {
            println!("PROBE replies_page1=error={message}");
            summary.push(json!({
                "capture": "replies_page1",
                "outcome": "error",
                "message": message,
            }));
        }
    }
    Ok(())
}
