//! Phase 4.3 upstream **evidence probe** (manual, network-using).
//!
//! This is not a test. It performs real requests to `www.youtube.com` through the production
//! [`ReqwestTransport`] — the same guarded transport, allowlist, resolver and size cap that Vydra
//! uses at runtime — and writes what came back to a directory for review. Run it only through
//! `.github/workflows/probe.yml` (`workflow_dispatch`), because the development sandbox has no
//! egress to YouTube.
//!
//! It exists so that a "recorded" fixture can mean what the fixture corpus says it means: captured
//! by Vydra, on a stated date, with a stated request shape and a stated redaction pass.
//!
//! Guarantees, enforced in code below:
//!
//! * the api key, the visitor id, cookies and every URL query string are removed **before** anything
//!   is written to disk (`redact`);
//! * a request that fails is recorded as a failure — nothing is synthesised to fill the gap;
//! * the probe never retries a non-2xx response and never attempts to look like a browser beyond the
//!   client profile the provider already ships (no PoToken, no `n-sig`, no consent-parameter tricks).
//!
//! Outputs (all in `VYDRA_PROBE_OUT`, default `probe-out/`):
//!
//! | File | Meaning |
//! | --- | --- |
//! | `summary.json` | request shapes, HTTP statuses, byte counts, latency, renderer counts, item summary |
//! | `search_page_1.json` | redacted first-page `/youtubei/v1/search` response |
//! | `search_page_2.json` | redacted continuation response (only if page 1 returned a token) |
//! | `search_mixed.json` | redacted response for a query expected to mix renderer kinds |
//! | `search_empty.json` | redacted response for a query expected to return nothing |
//! | `search_error.json` | redacted error envelope for a malformed request (evidence, not a feature) |

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

use vydra_providers::innertube::{
    ClientKind, ClientProfile, ContinuationRequest, InnertubeClient, InnertubeProvider,
    InnertubeProviderConfig, SearchRequest,
};
use vydra_providers::parse::find_continuation;
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReqwestTransport, SearchFilters,
    SearchKind, Strictness,
};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

/// Keys removed outright: identity, session and tracking material that has no evidentiary value for
/// a response-shape fixture.
const DROP_KEYS: &[&str] = &[
    "responseContext",
    "frameworkUpdates",
    "trackingParams",
    "clickTrackingParams",
    "visitorData",
    "apiKey",
    "consentCookie",
    "signatureCipher",
    "cipher",
    "playbackTracking",
    "adPlacements",
    "playerAds",
    "attestation",
    "serviceTrackingParams",
    "loggingContext",
    "sessionIndex",
    "delegatedSessionId",
];

/// Keys whose string value is a URL: the query string (signature, expiry, token) is stripped.
const URL_KEYS: &[&str] = &[
    "url",
    "baseUrl",
    "targetUrl",
    "thumbnailUrl",
    "webPageUrl",
    "canonicalBaseUrl",
];

fn main_context() -> ProviderContext {
    ProviderContext {
        deadline: Instant::now() + Duration::from_secs(60),
        request_id: "phase-4.3-probe".to_owned(),
        hl: "en".to_owned(),
        gl: "US".to_owned(),
        strictness: Strictness::Lenient,
    }
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}

/// Remove identity, tracking and URL-query material from a response body.
fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if DROP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if URL_KEYS.contains(&key.as_str())
                    && let Value::String(text) = &child
                {
                    let trimmed = text.split('?').next().unwrap_or_default().to_owned();
                    out.insert(key, Value::String(trimmed));
                    continue;
                }
                out.insert(key, redact(child));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        other => other,
    }
}

/// Mask anything that looks like a Google api key in free text (non-JSON bodies, error strings).
fn mask_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("AIza") {
        out.push_str(&rest[..at]);
        out.push_str("<redacted-key>");
        let tail = &rest[at + 4..];
        let skip = tail
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .map(|(index, c)| index + c.len_utf8())
            .last()
            .unwrap_or(0);
        rest = &rest[at + 4 + skip..];
    }
    out.push_str(rest);
    out
}

/// Count occurrences of a renderer key anywhere in the document.
fn count_key(value: &Value, key: &str) -> usize {
    match value {
        Value::Object(map) => {
            let here = usize::from(map.contains_key(key));
            here + map
                .values()
                .map(|child| count_key(child, key))
                .sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|child| count_key(child, key)).sum(),
        _ => 0,
    }
}

fn top_level_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort_unstable();
            keys
        }
        _ => Vec::new(),
    }
}

/// One recorded call: the redacted document plus the facts about it.
struct Recorded {
    label: &'static str,
    status: u16,
    bytes: usize,
    elapsed_ms: u128,
    body_keys: Vec<String>,
    payload: Option<Value>,
}

impl Recorded {
    fn summary(&self) -> Value {
        let mut entry = Map::new();
        entry.insert("label".into(), json!(self.label));
        entry.insert("status".into(), json!(self.status));
        entry.insert("bytes".into(), json!(self.bytes));
        entry.insert("elapsed_ms".into(), json!(self.elapsed_ms as u64));
        entry.insert("request_body_keys".into(), json!(self.body_keys));
        entry.insert(
            "request_url".into(),
            json!("https://www.youtube.com/youtubei/v1/search?prettyPrint=false&key=<redacted>"),
        );
        match &self.payload {
            Some(payload) => {
                entry.insert("top_level_keys".into(), json!(top_level_keys(payload)));
                entry.insert(
                    "video_renderers".into(),
                    json!(count_key(payload, "videoRenderer")),
                );
                entry.insert(
                    "channel_renderers".into(),
                    json!(count_key(payload, "channelRenderer")),
                );
                entry.insert(
                    "playlist_renderers".into(),
                    json!(count_key(payload, "playlistRenderer")),
                );
                entry.insert(
                    "shelf_renderers".into(),
                    json!(count_key(payload, "shelfRenderer")),
                );
                entry.insert(
                    "item_sections".into(),
                    json!(count_key(payload, "itemSectionRenderer")),
                );
                entry.insert(
                    "continuation_token_len".into(),
                    json!(find_continuation(payload).map(|token| token.len())),
                );
                entry.insert(
                    "estimated_results".into(),
                    payload
                        .get("estimatedResults")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            None => {
                entry.insert("top_level_keys".into(), Value::Null);
            }
        }
        Value::Object(entry)
    }
}

async fn record(
    client: &InnertubeClient,
    ctx: &ProviderContext,
    out: &Path,
    label: &'static str,
    body: Value,
) -> ProbeResult<Recorded> {
    let body_keys: Vec<String> = match body.as_object() {
        Some(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort_unstable();
            keys
        }
        None => Vec::new(),
    };

    let started = Instant::now();
    let (status, bytes) = client.post_raw("search", body, ctx).await?;
    let elapsed_ms = started.elapsed().as_millis();
    let raw_len = bytes.len();
    let payload = serde_json::from_slice::<Value>(&bytes).ok();

    match &payload {
        Some(parsed) => {
            let redacted = redact(parsed.clone());
            let path = out.join(format!("{label}.json"));
            fs::write(&path, serde_json::to_vec_pretty(&redacted)?)?;
            println!(
                "{label}: status={status} bytes={raw_len} elapsed_ms={elapsed_ms} -> {}",
                path.display()
            );
        }
        None => {
            let sample = mask_keys(&String::from_utf8_lossy(&bytes[..bytes.len().min(600)]));
            let path = out.join(format!("{label}.txt"));
            fs::write(&path, format!("status={status}\n\n{sample}\n"))?;
            println!(
                "{label}: status={status} bytes={raw_len} (not JSON) -> {}",
                path.display()
            );
        }
    }

    Ok(Recorded {
        label,
        status,
        bytes: raw_len,
        elapsed_ms,
        body_keys,
        payload,
    })
}

/// Summarise the provider-level mapping of a payload (what the public API would return).
fn item_summary(results: &vydra_domain::SearchResults) -> Value {
    let kinds: Vec<&str> = results.items.iter().map(|item| item.kind()).collect();
    let first = results
        .items
        .first()
        .map(|item| match item {
            vydra_domain::SearchItem::Video(video) => json!({
                "kind": "video",
                "id": video.id.as_str(),
                "title": video.title,
                "duration_seconds": video.duration_seconds,
                "view_count": video.view_count.as_ref().map(|count| count.value),
            }),
            vydra_domain::SearchItem::Channel(channel) => json!({
                "kind": "channel",
                "id": channel.id.as_str(),
                "title": channel.title,
                "handle": channel.handle,
            }),
            vydra_domain::SearchItem::Playlist(playlist) => json!({
                "kind": "playlist",
                "id": playlist.id.as_str(),
                "title": playlist.title,
            }),
        })
        .unwrap_or(Value::Null);

    json!({
        "items": results.items.len(),
        "kinds": kinds,
        "first": first,
        "next_cursor_len": results.next_cursor.as_ref().map(|token| token.len()),
        "estimated_results": results.estimated_results.as_ref().map(|count| count.value),
        "parse_warnings": results.provenance.parse_warnings,
    })
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let out = PathBuf::from(env_or("VYDRA_PROBE_OUT", "probe-out"));
    fs::create_dir_all(&out)?;
    let query = env_or("VYDRA_PROBE_QUERY", "captain jack");
    let mixed_query = env_or("VYDRA_PROBE_MIXED_QUERY", "veritasium");
    let empty_query = env_or("VYDRA_PROBE_EMPTY_QUERY", "zzqqxxvydraprobe9931");

    let transport = ReqwestTransport::new()?;
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        request_timeout: Duration::from_secs(15),
        ..Default::default()
    });
    let client = InnertubeClient::new(config.clone(), Arc::new(transport.clone()));
    let started_at = OffsetDateTime::now_utc().format(&Rfc3339)?;

    let ctx = main_context();
    let session = client.session(&ctx).await;

    let mut records: Vec<Value> = Vec::new();
    let mut notes: BTreeMap<String, Value> = BTreeMap::new();
    notes.insert("generated_at".into(), json!(started_at));
    notes.insert("probe_version".into(), json!(env!("CARGO_PKG_VERSION")));
    notes.insert("query".into(), json!(query));
    notes.insert("mixed_query".into(), json!(mixed_query));
    notes.insert("empty_query".into(), json!(empty_query));
    notes.insert("client_name".into(), json!(config.client.name));
    notes.insert("client_version".into(), json!(config.client.version));
    notes.insert("client_name_id".into(), json!(config.client.name_id));
    notes.insert("hl".into(), json!(ctx.hl));
    notes.insert("gl".into(), json!(ctx.gl));
    notes.insert("has_api_key".into(), json!(session.api_key.is_some()));
    notes.insert(
        "has_visitor_data".into(),
        json!(session.visitor_data.is_some()),
    );

    // ---- page 1 (primary query) -------------------------------------------------------------
    let page_1_body =
        serde_json::to_value(SearchRequest::new(client.context(&session, &ctx), query.clone()))?;
    let page_1 = record(&client, &main_context(), &out, "search_page_1", page_1_body).await?;
    let token = page_1
        .payload
        .as_ref()
        .and_then(find_continuation)
        .or_else(|| {
            std::env::var("VYDRA_PROBE_CURSOR")
                .ok()
                .filter(|token| !token.is_empty())
        });
    records.push(page_1.summary());

    // ---- page 2 (continuation of page 1) ----------------------------------------------------
    if let Some(token) = token {
        let page_2_body = serde_json::to_value(ContinuationRequest::new(
            client.context(&session, &ctx),
            token,
        ))?;
        let page_2 = record(&client, &main_context(), &out, "search_page_2", page_2_body).await?;
        records.push(page_2.summary());
    } else {
        notes.insert(
            "continuation".into(),
            json!("page 1 contained no continuation token; no second page requested"),
        );
    }

    // ---- mixed renderer kinds ---------------------------------------------------------------
    let mixed_body =
        serde_json::to_value(SearchRequest::new(client.context(&session, &ctx), mixed_query.clone()))?;
    let mixed = record(&client, &main_context(), &out, "search_mixed", mixed_body).await?;
    records.push(mixed.summary());

    // ---- empty result page ------------------------------------------------------------------
    let empty_body =
        serde_json::to_value(SearchRequest::new(client.context(&session, &ctx), empty_query.clone()))?;
    let empty = record(&client, &main_context(), &out, "search_empty", empty_body).await?;
    records.push(empty.summary());

    // ---- error envelope (deliberately malformed: no `query`) --------------------------------
    // Deliberately malformed: the context object alone, with no `query` member.
    let error_body = serde_json::to_value(client.context(&session, &ctx))?;
    let error = record(&client, &main_context(), &out, "search_error", error_body).await?;
    records.push(error.summary());

    // ---- provider-level mapping (what the public API would serve) --------------------------
    let provider = InnertubeProvider::new(config, Arc::new(transport));
    let provider_ctx = main_context();
    let request = ProviderRequest::Search {
        query: query.clone(),
        kind: SearchKind::All,
        filters: SearchFilters::default(),
        cursor: None,
        hl: provider_ctx.hl.clone(),
        gl: provider_ctx.gl.clone(),
        limit: 20,
    };
    let mapped = match provider.fetch(request, &provider_ctx).await {
        Ok(ProviderResponse::Search(results)) => json!({
            "ok": true,
            "endpoint": results.provenance.endpoint,
            "summary": item_summary(&results),
        }),
        Ok(_) => json!({"ok": false, "error": "unexpected response variant"}),
        Err(err) => json!({"ok": false, "code": err.code(), "error": err.to_string()}),
    };
    notes.insert("provider_mapping".into(), mapped);

    let summary = json!({
        "probe": notes,
        "records": records,
    });
    let path = out.join("summary.json");
    fs::write(&path, serde_json::to_vec_pretty(&summary)?)?;
    println!("summary -> {}", path.display());

    // Workflow annotations: short, greppable, no payload content.
    for record in summary["records"].as_array().into_iter().flatten() {
        println!(
            "::notice::probe {} status={} bytes={} token_len={}",
            record["label"], record["status"], record["bytes"], record["continuation_token_len"]
        );
    }
    println!(
        "::notice::probe provider_mapping ok={}",
        summary["probe"]["provider_mapping"]["ok"]
    );
    Ok(())
}
