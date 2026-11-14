//! Phase 5A upstream **player evidence probe** (manual, network-using).
//!
//! This is not a test. It performs a small, fixed set of real requests to `www.youtube.com` through
//! the production [`ReqwestTransport`] and [`InnertubeClient`] — the same guarded transport,
//! allowlist, resolver, redirect policy, deadline and size cap that Vydra uses at runtime, with the
//! same session bootstrap and the same [`PlayerRequest`] body. Run it only through
//! `.github/workflows/probe.yml` (`workflow_dispatch`) or the gated CI job described in
//! `docs/reports/PHASE_5A_REPORT.md`, because the development sandbox has no egress to YouTube.
//!
//! It exists so that "recorded fixture" can mean what the fixture corpus says it means: captured by
//! Vydra, on a stated date, with a stated request shape and a stated redaction pass.
//!
//! What it deliberately does **not** do:
//!
//! * no ID enumeration — four fixed requests against three fixed video ids (one is repeated with the
//!   opposite check flags so the flag's effect is measurable, not assumed);
//! * no consent/cookie/sign-in trick, no PoToken, no `n-sig`, no playback URL request;
//! * a request that fails is recorded as a failure — nothing is synthesised to fill the gap, and no
//!   body is written when the redaction self-check fails.
//!
//! Redaction, enforced in code below:
//!
//! * identity/session/tracking keys are dropped (`responseContext`, `adPlacements`, …);
//! * the cipher blobs (`signatureCipher`, `cipher`) are dropped outright — Vydra never deciphers;
//! * every string value that *is* a URL has its query string removed, which is where `sig`, `n`,
//!   `expire`, `ip` and the key live;
//! * a self-check rejects a body that still matches an api-key or signature marker.
//!
//! Outputs (all in `VYDRA_PROBE_OUT`, default `probe-out/`):
//!
//! | File | Meaning |
//! | --- | --- |
//! | `summary.json` | request shapes, HTTP statuses, headers of record, byte counts, latency, verdicts, provider mapping |
//! | `player_public.json` | redacted `/youtubei/v1/player` response for a long-standing public video |
//! | `player_public_no_checks.json` | same video, `contentCheckOk=false`/`racyCheckOk=false` |
//! | `player_captions.json` | redacted response for a public video known to carry caption tracks |
//! | `player_missing.json` | redacted response for a syntactically valid id that does not resolve |

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

use vydra_common::VideoId;
use vydra_providers::innertube::{
    ClientKind, ClientProfile, InnertubeClient, InnertubeProvider, InnertubeProviderConfig,
    PlayerRequest,
};
use vydra_providers::{
    Provider, ProviderContext, ProviderRequest, ProviderResponse, ReqwestTransport, Strictness,
};

type ProbeResult<T> = Result<T, Box<dyn Error>>;

/// Keys removed outright: identity, session, advertising and tracking material. None of it has
/// evidentiary value for a response-shape fixture, and some of it is session-scoped.
const DROP_KEYS: &[&str] = &[
    "responseContext",
    "frameworkUpdates",
    "trackingParams",
    "clickTrackingParams",
    "visitorData",
    "apiKey",
    "consentCookie",
    "playbackTracking",
    "adPlacements",
    "playerAds",
    "attestation",
    "serviceTrackingParams",
    "loggingContext",
    "sessionIndex",
    "delegatedSessionId",
    "signatureCipher",
    "cipher",
];

/// Response-level advertising and promotion surfaces. They carry no evidence about the player
/// document (a mealbar promo, an ad-break token) and they are where the remaining tracking URLs live.
/// `playabilityStatus.messages` is a *different* field and is kept.
const DROP_TOP_LEVEL_KEYS: &[&str] = &["messages", "adBreakHeartbeatParams"];

/// Markers that must not survive redaction. `AIza` is the api-key prefix; the rest only appear in
/// playback URL query strings, which are stripped.
const SECRET_MARKERS: &[&str] = &["AIza", "sigh=", "&sig=", "?sig=", "expire="];

fn context() -> ProviderContext {
    ProviderContext {
        deadline: Instant::now() + Duration::from_secs(120),
        request_id: "phase-5a-probe".to_owned(),
        hl: "en".to_owned(),
        gl: "US".to_owned(),
        strictness: Strictness::Lenient,
    }
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
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

/// Bounded, single-line, key-masked form of a message that may quote upstream text.
fn sanitize_text(text: &str) -> String {
    let masked = mask_keys(text);
    masked
        .chars()
        .take(400)
        .collect::<String>()
        .replace('\n', " ")
}

/// Drop the query string of a value that is itself a URL. `sig`, `n`, `expire`, `ip` and the key all
/// live there; the path (and therefore the shape) is kept as evidence.
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
        Value::String(text) if text.starts_with("http") => Value::String(strip_query(&text)),
        other => other,
    }
}

/// Redact the response root: drop the promotion surfaces first, then walk the document.
fn redact_root(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let filtered: Map<String, Value> = map
                .into_iter()
                .filter(|(key, _)| !DROP_TOP_LEVEL_KEYS.contains(&key.as_str()))
                .collect();
            redact(Value::Object(filtered))
        }
        other => redact(other),
    }
}

/// Self-check on the redacted document. A false result withholds the body instead of publishing it.
///
/// Two rules: no api-key or signature marker may survive, and no string value may still carry a
/// query string (relative `apiUrl` paths included — those are where promo params live).
fn redacted_ok(redacted: &Value) -> bool {
    let text = redacted.to_string();
    if SECRET_MARKERS.iter().any(|marker| text.contains(marker)) {
        return false;
    }
    !strings_with_query(redacted)
}

/// Whether any string value still looks like a URL carrying a query string.
fn strings_with_query(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.values().any(strings_with_query),
        Value::Array(items) => items.iter().any(strings_with_query),
        Value::String(text) => {
            let looks_like_url = text.contains("http") || text.starts_with('/');
            looks_like_url && text.contains('?')
        }
        _ => false,
    }
}

fn object_keys(value: &Value) -> Vec<String> {
    match value.as_object() {
        Some(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort_unstable();
            keys
        }
        None => Vec::new(),
    }
}

fn array_len(value: Option<&Value>) -> Option<usize> {
    value.and_then(Value::as_array).map(Vec::len)
}

/// Structural classification of one player response: what upstream said, without any URL material.
fn classify(payload: &Value) -> Value {
    let playability = payload.get("playabilityStatus");
    let details = payload.get("videoDetails");
    let streaming = payload.get("streamingData");
    let tracks = payload
        .pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")
        .and_then(Value::as_array);
    let languages: Vec<Value> = tracks
        .map(|tracks| {
            tracks
                .iter()
                .map(|track| {
                    json!({
                        "language_code": track.get("languageCode"),
                        "kind": track.get("kind"),
                        "is_translatable": track.get("isTranslatable"),
                        "has_name": track
                            .pointer("/name/simpleText")
                            .or_else(|| track.pointer("/name/runs/0/text"))
                            .is_some(),
                        "translation_languages": array_len(track.get("translationLanguages")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let microformat = payload
        .get("microformat")
        .and_then(|micro| micro.get("playerMicroformatRenderer"));

    json!({
        "playability": playability.map(|status| json!({
            "status": status.get("status"),
            "reason": status.get("reason"),
            "messages": array_len(status.get("messages")),
            "has_error_screen": status.get("errorScreen").is_some(),
        })),
        "video_details": details.map(|details| json!({
            "video_id": details.get("videoId"),
            "title": details.get("title"),
            "author": details.get("author"),
            "channel_id": details.get("channelId"),
            "length_seconds": details.get("lengthSeconds"),
            "view_count": details.get("viewCount"),
            "keywords": array_len(details.get("keywords")),
            "thumbnail_versions": array_len(details.pointer("/thumbnail/thumbnails")),
            "is_live_content": details.get("isLiveContent"),
            "is_live_now": details.get("isLiveNow"),
            "is_upcoming": details.get("isUpcoming"),
            "short_description_len": details
                .get("shortDescription")
                .and_then(Value::as_str)
                .map(str::len),
        })),
        "streaming_data": streaming.map(|streaming| json!({
            "formats": array_len(streaming.get("formats")),
            "adaptive_formats": array_len(streaming.get("adaptiveFormats")),
            "has_hls_manifest": streaming.get("hlsManifestUrl").is_some(),
            "has_dash_manifest": streaming.get("dashManifestUrl").is_some(),
        })),
        "captions": {
            "present": payload.get("captions").is_some(),
            "track_count": tracks.map(Vec::len),
            "tracks": languages,
        },
        "microformat": microformat.map(|micro| json!({
            "publish_date": micro.get("publishDate"),
            "upload_date": micro.get("uploadDate"),
            "category": micro.get("category"),
            "owner_channel_name": micro.get("ownerChannelName"),
            "length_seconds": micro.get("lengthSeconds"),
            "is_live": micro.get("liveBroadcastDetails").is_some(),
            "has_description": micro.get("description").is_some(),
            "thumbnail_versions": array_len(micro.pointer("/thumbnail/thumbnails")),
        })),
        "storyboards": {
            "present": payload.get("storyboards").is_some(),
            "has_spec": payload
                .pointer("/storyboards/playerStoryboardSpecRenderer/spec")
                .is_some(),
        },
    })
}

/// One recorded attempt: the redacted document plus the facts about it.
struct Capture {
    label: String,
    video_id: String,
    body_keys: Vec<String>,
    payload_keys: Vec<String>,
    status: u16,
    content_type: Option<String>,
    upstream_date: Option<String>,
    bytes: usize,
    elapsed_ms: u128,
    is_json: bool,
    redaction_ok: bool,
    verdict: Value,
}

impl Capture {
    fn summary(&self) -> Value {
        json!({
            "label": self.label,
            "video_id": self.video_id,
            "method": "POST",
            "request_url": "https://www.youtube.com/youtubei/v1/player?prettyPrint=false&key=<redacted>",
            "request_body_keys": self.body_keys,
            "response_top_level_keys": self.payload_keys,
            "status": self.status,
            "content_type": self.content_type,
            "upstream_date": self.upstream_date,
            "bytes": self.bytes,
            "elapsed_ms": self.elapsed_ms as u64,
            "json": self.is_json,
            "redaction_ok": self.redaction_ok,
            "verdict": self.verdict,
        })
    }
}

fn header(response: &vydra_providers::TransportResponse, name: &str) -> Option<String> {
    response.header(name).map(str::to_owned)
}

async fn record(
    client: &InnertubeClient,
    ctx: &ProviderContext,
    out: &Path,
    label: &str,
    video_id: &str,
    content_check_ok: bool,
    racy_check_ok: bool,
) -> ProbeResult<Capture> {
    let session = client.session(ctx).await;
    let body = serde_json::to_value(PlayerRequest::with_checks(
        client.context(&session, ctx),
        video_id,
        content_check_ok,
        racy_check_ok,
    ))?;
    let body_keys = object_keys(&body);

    let started = Instant::now();
    let response = match client.post_raw_capture("player", body, ctx).await {
        Ok(response) => response,
        Err(error) => {
            // A transport-level refusal is evidence too: record the class, never a body.
            println!("{label}: transport error {}", error.code());
            return Ok(Capture {
                label: label.to_owned(),
                video_id: video_id.to_owned(),
                body_keys,
                payload_keys: Vec::new(),
                status: 0,
                content_type: None,
                upstream_date: None,
                bytes: 0,
                elapsed_ms: started.elapsed().as_millis(),
                is_json: false,
                redaction_ok: true,
                verdict: json!({"transport_error": error.code()}),
            });
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    let raw_len = response.body.len();
    let parsed = serde_json::from_slice::<Value>(&response.body).ok();

    let content_type = header(&response, "content-type");
    let upstream_date = header(&response, "date");

    let verdict = match &parsed {
        Some(payload) => classify(payload),
        None => json!({"note": "response body is not JSON"}),
    };
    let mut redaction_ok = true;

    if let Some(payload) = &parsed {
        let redacted = redact_root(payload.clone());
        redaction_ok = redacted_ok(&redacted);
        if redaction_ok {
            let path = out.join(format!("player_{label}.json"));
            fs::write(&path, serde_json::to_vec_pretty(&redacted)?)?;
            println!(
                "{label}: status={} bytes={raw_len} elapsed_ms={elapsed_ms} -> {}",
                response.status,
                path.display()
            );
        } else {
            println!("{label}: REDACTION SELF-CHECK FAILED — body withheld");
            println!(
                "::error title=redaction::{label} matched a secret marker after redaction; body withheld"
            );
        }
    } else {
        let sample = sanitize_text(&String::from_utf8_lossy(
            &response.body[..response.body.len().min(600)],
        ));
        let path = out.join(format!("player_{label}.txt"));
        fs::write(
            &path,
            format!(
                "status={}\ncontent-type={}\n\n{}\n",
                response.status,
                content_type.as_deref().unwrap_or(""),
                sample
            ),
        )?;
        println!(
            "{label}: status={} bytes={raw_len} (not JSON) -> {}",
            response.status,
            path.display()
        );
    }

    Ok(Capture {
        label: label.to_owned(),
        video_id: video_id.to_owned(),
        body_keys,
        payload_keys: parsed.as_ref().map(object_keys).unwrap_or_default(),
        status: response.status,
        content_type,
        upstream_date,
        bytes: raw_len,
        elapsed_ms,
        is_json: parsed.is_some(),
        redaction_ok,
        verdict,
    })
}

/// What the provider would return for the same id, i.e. the public-API projection.
async fn provider_mapping(provider: &InnertubeProvider, video_id: &str) -> Value {
    let id = match VideoId::parse(video_id) {
        Ok(id) => id,
        Err(error) => return json!({"ok": false, "stage": "id", "error": error.to_string()}),
    };
    let ctx = context();
    let request = ProviderRequest::Video {
        id,
        hl: ctx.hl.clone(),
        gl: ctx.gl.clone(),
    };
    match provider.fetch(request, &ctx).await {
        Ok(ProviderResponse::Video(document)) => json!({
            "ok": true,
            "endpoint": document.provenance.endpoint,
            "availability": document.availability,
            "title": document.summary.title,
            "duration_seconds": document.summary.duration_seconds,
            "view_count": document.summary.view_count.map(|count| count.value),
            "channel_id": document.summary.channel_id.as_ref().map(|id| id.as_str()),
            "live_state": document.summary.live_state,
            "thumbnails": document.summary.thumbnails.len(),
            "category": document.category,
            "keywords": document.keywords.len(),
            "chapters": document.chapters.len(),
            "caption_tracks": document.caption_tracks.len(),
            "formats": document.formats.len(),
            "parse_warnings": document.provenance.parse_warnings,
        }),
        Ok(_) => json!({"ok": false, "error": "unexpected response variant"}),
        Err(error) => json!({
            "ok": false,
            "code": error.code(),
            "message": sanitize_text(&error.to_string()),
        }),
    }
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let out = PathBuf::from(env_or("VYDRA_PROBE_OUT", "probe-out"));
    fs::create_dir_all(&out)?;

    let public_id = env_or("VYDRA_PROBE_VIDEO_PUBLIC", "dQw4w9WgXcQ");
    let captions_id = env_or("VYDRA_PROBE_VIDEO_CAPTIONS", "9Umm2MAKsMc");
    let missing_id = env_or("VYDRA_PROBE_VIDEO_MISSING", "AAAAAAAAAAA");

    let transport = ReqwestTransport::new()?;
    let config = Arc::new(InnertubeProviderConfig {
        client: ClientProfile::from_kind(ClientKind::Web, None),
        request_timeout: Duration::from_secs(20),
        ..Default::default()
    });
    let client = InnertubeClient::new(config.clone(), Arc::new(transport.clone()));
    let started_at = OffsetDateTime::now_utc().format(&Rfc3339)?;

    let ctx = context();
    let session = client.session(&ctx).await;

    let mut notes: BTreeMap<String, Value> = BTreeMap::new();
    notes.insert("generated_at".into(), json!(started_at));
    notes.insert("probe_version".into(), json!(env!("CARGO_PKG_VERSION")));
    notes.insert(
        "run_id".into(),
        json!(std::env::var("GITHUB_RUN_ID").unwrap_or_default()),
    );
    notes.insert(
        "commit".into(),
        json!(std::env::var("GITHUB_SHA").unwrap_or_default()),
    );
    notes.insert("endpoint".into(), json!("POST /youtubei/v1/player"));
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
    notes.insert("public_video_id".into(), json!(public_id));
    notes.insert("captions_video_id".into(), json!(captions_id));
    notes.insert("missing_video_id".into(), json!(missing_id));

    let mut records: Vec<Value> = Vec::new();

    // ---- a long-standing public video, the flags the provider sends --------------------------
    let public = record(&client, &ctx, &out, "public", &public_id, true, true).await?;
    records.push(public.summary());

    // ---- the same video with the opposite check flags: does the verdict depend on them? ------
    let public_no_checks = record(
        &client,
        &ctx,
        &out,
        "public_no_checks",
        &public_id,
        false,
        false,
    )
    .await?;
    records.push(public_no_checks.summary());

    // ---- a public video with caption tracks -------------------------------------------------
    let captions = record(&client, &ctx, &out, "captions", &captions_id, true, true).await?;
    records.push(captions.summary());

    // ---- a syntactically valid id that should not resolve -----------------------------------
    let missing = record(&client, &ctx, &out, "missing", &missing_id, true, true).await?;
    records.push(missing.summary());

    // ---- provider-level mapping (what the public API would serve) ---------------------------
    let provider = InnertubeProvider::new(config, Arc::new(transport));
    let mut mapping: BTreeMap<String, Value> = BTreeMap::new();
    mapping.insert(
        "public".into(),
        provider_mapping(&provider, &public_id).await,
    );
    mapping.insert(
        "captions".into(),
        provider_mapping(&provider, &captions_id).await,
    );
    mapping.insert(
        "missing".into(),
        provider_mapping(&provider, &missing_id).await,
    );
    notes.insert("provider_mapping".into(), json!(mapping));

    let summary = json!({
        "probe": notes,
        "records": records,
    });
    let path = out.join("summary.json");
    fs::write(&path, serde_json::to_vec_pretty(&summary)?)?;
    println!("summary -> {}", path.display());

    // Workflow annotations: short, greppable, no payload content.
    for record in summary["records"].as_array().into_iter().flatten() {
        // The verdict is the evidence; keep it on the check run so it is visible even if the capture
        // transport (artifact or branch push) fails.
        println!(
            "::notice::player probe {} id={} status={} bytes={} json={} verdict={} reason={} formats={} captions={} video_details={}",
            record["label"],
            record["video_id"],
            record["status"],
            record["bytes"],
            record["json"],
            record["verdict"]["playability"]["status"],
            record["verdict"]["playability"]["reason"],
            record["verdict"]["streaming_data"]["adaptive_formats"],
            record["verdict"]["captions"]["track_count"],
            record["verdict"]["video_details"]["channel_id"]
        );
    }
    for (label, mapped) in mapping {
        println!(
            "::notice::player provider_mapping {label} ok={} code={}",
            mapped["ok"], mapped["code"]
        );
    }
    Ok(())
}
