//! InnerTube HTTP client and session bootstrap.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;
use url::Url;

use super::{
    ClientContext, ClientProfile, INNERTUBE_BASE, InnertubeProviderConfig, SW_JS_DATA_URL,
    YOUTUBE_HOME,
};
use crate::parse::find_json_string_field;
use crate::transport::{HttpMethod, Transport, TransportError, TransportRequest};
use crate::{ProviderContext, ProviderError};

/// Bootstrap value set. Every field is optional because each piece can be missing: a session is
/// still usable with only an api key (the reference clients fall back the same way).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionData {
    pub api_key: Option<String>,
    pub visitor_data: Option<String>,
    pub client_version: Option<String>,
    pub fetched_at: Option<Instant>,
}

impl SessionData {
    fn is_fresh(&self, ttl: Duration) -> bool {
        match self.fetched_at {
            Some(at) => at.elapsed() < ttl,
            None => false,
        }
    }
}

/// InnerTube request layer. One instance per provider; cheap to clone through an `Arc`.
///
/// `Debug` is implemented by hand: the transport is `dyn Transport` and is intentionally opaque, so
/// it is reported by presence rather than by contents.
pub struct InnertubeClient {
    transport: Arc<dyn Transport>,
    config: Arc<InnertubeProviderConfig>,
    session: Mutex<Option<SessionData>>,
    refresh_lock: Mutex<()>,
}

impl std::fmt::Debug for InnertubeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `session` is a tokio mutex; `try_lock` keeps `Debug` synchronous and non-blocking.
        let has_session = self
            .session
            .try_lock()
            .map(|session| session.is_some())
            .unwrap_or(false);
        f.debug_struct("InnertubeClient")
            .field("client", &self.config.client)
            .field("has_session", &has_session)
            .finish_non_exhaustive()
    }
}

impl InnertubeClient {
    pub fn new(config: Arc<InnertubeProviderConfig>, transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            config,
            session: Mutex::new(None),
            refresh_lock: Mutex::new(()),
        }
    }

    pub fn profile(&self) -> &ClientProfile {
        &self.config.client
    }

    pub async fn invalidate_session(&self) {
        let mut guard = self.session.lock().await;
        *guard = None;
        debug!(provider = %self.config.provider_id, "session invalidated");
    }

    /// Current session, refreshing at most once at a time (single-flight).
    pub async fn session(&self, ctx: &ProviderContext) -> SessionData {
        {
            let guard = self.session.lock().await;
            if let Some(session) = guard.as_ref()
                && session.is_fresh(self.config.session_ttl)
            {
                return session.clone();
            }
        }

        let _refresh = self.refresh_lock.lock().await;
        {
            // Another task may have refreshed while we waited for the lock.
            let guard = self.session.lock().await;
            if let Some(session) = guard.as_ref()
                && session.is_fresh(self.config.session_ttl)
            {
                return session.clone();
            }
        }

        let session = self.refresh_session(ctx).await;
        let mut guard = self.session.lock().await;
        *guard = Some(session.clone());
        session
    }

    async fn refresh_session(&self, ctx: &ProviderContext) -> SessionData {
        let mut session = SessionData {
            api_key: self
                .config
                .api_key_override
                .clone()
                .or_else(|| self.config.client.api_key.clone()),
            client_version: Some(self.config.client.version.clone()),
            ..Default::default()
        };

        if let Some(value) = self.fetch_json(SW_JS_DATA_URL, ctx).await {
            session.api_key = find_api_key(&value).or(session.api_key);
            session.visitor_data = find_field(&value, "visitorData");
            session.client_version = find_field(&value, "clientVersion").or(session.client_version);
        } else if let Some(html) = self.fetch_text(YOUTUBE_HOME, ctx).await {
            session.api_key =
                find_json_string_field(&html, "INNERTUBE_API_KEY").or(session.api_key);
            session.visitor_data = find_json_string_field(&html, "VISITOR_DATA")
                .or_else(|| find_json_string_field(&html, "visitorData"));
            session.client_version = find_json_string_field(&html, "INNERTUBE_CLIENT_VERSION")
                .or(session.client_version);
        }

        session.fetched_at = Some(Instant::now());
        debug!(
            provider = %self.config.provider_id,
            has_api_key = session.api_key.is_some(),
            has_visitor = session.visitor_data.is_some(),
            "session refreshed"
        );
        session
    }

    fn headers(&self, session: &SessionData, ctx: &ProviderContext) -> Vec<(String, String)> {
        let profile = &self.config.client;
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            (
                "x-youtube-client-name".to_owned(),
                profile.name_id.to_owned(),
            ),
            (
                "x-youtube-client-version".to_owned(),
                session
                    .client_version
                    .clone()
                    .unwrap_or_else(|| profile.version.clone()),
            ),
            ("origin".to_owned(), "https://www.youtube.com".to_owned()),
            ("referer".to_owned(), "https://www.youtube.com/".to_owned()),
            ("accept-language".to_owned(), format!("{},en;q=0.8", ctx.hl)),
        ];
        if let Some(user_agent) = &profile.user_agent {
            headers.push(("user-agent".to_owned(), user_agent.clone()));
        }
        if let Some(visitor) = &session.visitor_data {
            headers.push(("x-goog-visitor-id".to_owned(), visitor.clone()));
        }
        if let Some(cookie) = &self.config.consent_cookie {
            headers.push(("cookie".to_owned(), format!("CONSENT={cookie}")));
        }
        headers
    }

    fn timeout_for(&self, ctx: &ProviderContext) -> Result<Duration, ProviderError> {
        let remaining = ctx.remaining();
        if remaining.is_zero() {
            return Err(ProviderError::Timeout);
        }
        Ok(self.config.request_timeout.min(remaining))
    }

    /// Execute a request with an explicit session.
    ///
    /// Session bootstrap must **not** recurse through [`Self::session`], so the bootstrap calls pass
    /// an empty session here (only the client profile headers are used, no visitor id).
    async fn send(
        &self,
        url: &str,
        ctx: &ProviderContext,
        body: Option<Bytes>,
        session: &SessionData,
    ) -> Result<(crate::transport::TransportResponse, Duration), ProviderError> {
        let timeout = self.timeout_for(ctx)?;
        let parsed = Url::parse(url).map_err(|err| ProviderError::Transport {
            message: format!("invalid endpoint url: {err}"),
        })?;
        let request = TransportRequest {
            method: if body.is_some() {
                HttpMethod::Post
            } else {
                HttpMethod::Get
            },
            url: parsed,
            headers: self.headers(session, ctx),
            body,
            timeout,
            max_response_bytes: self.config.max_response_bytes,
        };

        let started = Instant::now();
        let response = self
            .transport
            .execute(request)
            .await
            .map_err(|err| map_transport_error(err, self.config.request_timeout))?;
        Ok((response, started.elapsed()))
    }

    async fn fetch_json(&self, url: &str, ctx: &ProviderContext) -> Option<Value> {
        let session = SessionData {
            client_version: Some(self.config.client.version.clone()),
            ..Default::default()
        };
        let (response, _) = self.send(url, ctx, None, &session).await.ok()?;
        if !response.is_success() {
            return None;
        }
        response.json().ok()
    }

    async fn fetch_text(&self, url: &str, ctx: &ProviderContext) -> Option<String> {
        let session = SessionData {
            client_version: Some(self.config.client.version.clone()),
            ..Default::default()
        };
        let (response, _) = self.send(url, ctx, None, &session).await.ok()?;
        if !response.is_success() {
            return None;
        }
        String::from_utf8(response.body.to_vec()).ok()
    }

    /// Endpoint URL, including the session API key when the bootstrap produced one.
    ///
    /// InnerTube takes the browser key as a `key` **query parameter** — yt-dlp builds
    /// `{'key': api_key, 'prettyPrint': 'false'}` as the query (`_base.py:858-866`), and MediaServiceCore
    /// keeps `Session.apiKey` for the same purpose. It is never a header and never part of the body.
    ///
    /// A value that is not shaped like a key is dropped instead of being interpolated: the key comes
    /// from a response body, and concatenating it blindly would let a hostile bootstrap inject query
    /// parameters.
    fn endpoint_url(&self, endpoint: &str, session: &SessionData) -> String {
        let mut url = format!("{INNERTUBE_BASE}/{endpoint}?prettyPrint=false");
        match session.api_key.as_deref() {
            Some(key) if looks_like_api_key(key) => {
                url.push_str("&key=");
                url.push_str(key);
            }
            Some(_) => {
                tracing::warn!(
                    provider = %self.config.provider_id,
                    "session api key has an unexpected shape; sending a keyless request"
                );
            }
            None => {}
        }
        url
    }

    /// POST a JSON body to an InnerTube endpoint and return `(parsed body, elapsed)`.
    pub async fn post_json(
        &self,
        endpoint: &str,
        body: Value,
        ctx: &ProviderContext,
    ) -> Result<(Value, Duration), ProviderError> {
        let encoded = serde_json::to_vec(&body).map_err(|err| ProviderError::InvalidRequest {
            field: "body".into(),
            issue: format!("serialize: {err}"),
        })?;

        let session = self.session(ctx).await;
        let url = self.endpoint_url(endpoint, &session);
        let (response, elapsed) = self
            .send(&url, ctx, Some(Bytes::from(encoded)), &session)
            .await?;

        if !response.is_success() {
            let message = String::from_utf8_lossy(&response.body)
                .chars()
                .take(200)
                .collect::<String>();
            return Err(match response.status {
                401 | 403 => {
                    // Session is likely stale or the client is being challenged.
                    self.invalidate_session().await;
                    ProviderError::Unauthorized { message }
                }
                404 => ProviderError::NotFound {
                    id: endpoint.to_owned(),
                    message,
                },
                429 => ProviderError::RateLimited {
                    retry_after: response.retry_after(),
                },
                status => ProviderError::Upstream { status, message },
            });
        }

        let value = response.json().map_err(|err| ProviderError::Parse {
            message: format!("invalid json from /{endpoint}: {err}"),
        })?;
        Ok((value, elapsed))
    }

    /// Execute an endpoint call and return the raw status and body **without** interpreting it.
    ///
    /// Added for the Phase 4.3 upstream evidence probe: an error envelope is evidence too, and
    /// `post_json` maps a non-success status to a `ProviderError` whose message keeps only the first
    /// 200 bytes. The probe needs the bytes.
    ///
    /// The request is built exactly like [`Self::post_json`] (same URL, key placement, headers,
    /// timeout and body cap); only the response handling differs. A `401`/`403` does **not**
    /// invalidate the session here — the caller decides whether the status means "stale session".
    pub async fn post_raw(
        &self,
        endpoint: &str,
        body: Value,
        ctx: &ProviderContext,
    ) -> Result<(u16, Bytes), ProviderError> {
        let response = self.post_raw_capture(endpoint, body, ctx).await?;
        Ok((response.status, response.body))
    }

    /// The whole uninterpreted response of a raw POST, including headers and round-trip time.
    ///
    /// Exists for the evidence probes (`examples/probe_search.rs`, `examples/probe_player.rs`), which
    /// must record `content-type`, `date` and latency next to the bytes. It goes through the same
    /// guarded transport, session and header construction as [`Self::post_raw`] — there is no
    /// second HTTP path — and it never retries.
    pub async fn post_raw_capture(
        &self,
        endpoint: &str,
        body: Value,
        ctx: &ProviderContext,
    ) -> Result<crate::transport::TransportResponse, ProviderError> {
        let encoded = serde_json::to_vec(&body).map_err(|err| ProviderError::InvalidRequest {
            field: "body".into(),
            issue: format!("serialize: {err}"),
        })?;

        let session = self.session(ctx).await;
        let url = self.endpoint_url(endpoint, &session);
        let (response, _) = self
            .send(&url, ctx, Some(Bytes::from(encoded)), &session)
            .await?;
        Ok(response)
    }

    /// Build the shared `context` object for a session.
    ///
    /// Typed since Phase 4.3: one source for every endpoint (see `request::ClientContext`), so the
    /// search and player bodies cannot drift apart.
    pub fn context(&self, session: &SessionData, ctx: &ProviderContext) -> ClientContext {
        ClientContext::new(&self.config.client, session, &ctx.hl, &ctx.gl)
    }
}

fn map_transport_error(err: TransportError, configured: Duration) -> ProviderError {
    match err {
        TransportError::Timeout(_) => ProviderError::Timeout,
        TransportError::BodyTooLarge { limit } => ProviderError::Parse {
            message: format!("response exceeded {limit} bytes"),
        },
        other => ProviderError::Transport {
            message: format!("{other} (configured timeout {configured:?})"),
        },
    }
}

fn find_field(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(Value::as_str)
                && !found.is_empty()
            {
                return Some(found.to_owned());
            }
            map.values().find_map(|child| find_field(child, key))
        }
        Value::Array(items) => items.iter().find_map(|child| find_field(child, key)),
        _ => None,
    }
}

/// Shape of a browser key as it appears in `sw.js_data` or the watch page.
///
/// The previous bound (`len() > 20`) rejected the very keys the fixtures and the documented
/// shape use: `AIza` plus a 16-character suffix is a valid test key and 20 characters long.
fn looks_like_api_key(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("AIza") else {
        return false;
    };
    (12..=60).contains(&rest.len())
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn find_api_key(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if looks_like_api_key(text) => Some(text.clone()),
        Value::Object(map) => map.values().find_map(find_api_key),
        Value::Array(items) => items.iter().find_map(find_api_key),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReplayTransport;
    use crate::Strictness;
    use serde_json::json;

    fn ctx() -> ProviderContext {
        ProviderContext {
            deadline: Instant::now() + Duration::from_secs(5),
            request_id: "test".into(),
            hl: "en".into(),
            gl: "US".into(),
            strictness: Strictness::Lenient,
        }
    }

    fn client(transport: Arc<dyn Transport>) -> InnertubeClient {
        let config = Arc::new(InnertubeProviderConfig {
            api_key_override: Some("AIzaTESTKEY".into()),
            ..Default::default()
        });
        InnertubeClient::new(config, transport)
    }

    #[test]
    fn finds_nested_values() {
        let value =
            json!({"a": [{"b": {"visitorData": "VISITOR"}}], "key": "AIzaABCDEFGHIJKLMNOP"});
        assert_eq!(
            find_field(&value, "visitorData").as_deref(),
            Some("VISITOR")
        );
        assert_eq!(
            find_api_key(&value).as_deref(),
            Some("AIzaABCDEFGHIJKLMNOP")
        );
    }

    #[test]
    fn endpoint_url_carries_the_session_key_as_a_query_parameter() {
        let transport = Arc::new(ReplayTransport::new(vec![]));
        let client = client(transport);
        let session = SessionData {
            api_key: Some("AIzaSWKEY0123456789".into()),
            ..Default::default()
        };
        let url = client.endpoint_url("search", &session);
        assert_eq!(
            url,
            "https://www.youtube.com/youtubei/v1/search?prettyPrint=false&key=AIzaSWKEY0123456789"
        );
    }

    #[test]
    fn endpoint_url_omits_the_key_when_the_session_has_none() {
        let transport = Arc::new(ReplayTransport::new(vec![]));
        let client = client(transport);
        let url = client.endpoint_url("player", &SessionData::default());
        assert_eq!(
            url,
            "https://www.youtube.com/youtubei/v1/player?prettyPrint=false"
        );
        assert!(!url.contains("key="));
    }

    #[test]
    fn endpoint_url_refuses_to_interpolate_a_hostile_key() {
        let transport = Arc::new(ReplayTransport::new(vec![]));
        let client = client(transport);
        for hostile in ["x&key=evil", "AIza ok", "AIza/../etc", ""] {
            let session = SessionData {
                api_key: Some(hostile.to_owned()),
                ..Default::default()
            };
            let url = client.endpoint_url("search", &session);
            assert!(!url.contains("evil"), "injected with {hostile:?}");
            assert!(!url.contains("key="), "appended {hostile:?}");
        }
    }

    #[tokio::test]
    async fn session_uses_sw_js_data_when_available() {
        let sw = json!([[{"visitorData": "VIS123"}], "AIzaSWKEY0123456789"]);
        let transport = Arc::new(ReplayTransport::json(Bytes::from(sw.to_string())));
        let client = client(transport.clone());

        let session = client.session(&ctx()).await;
        assert_eq!(session.visitor_data.as_deref(), Some("VIS123"));
        assert_eq!(session.api_key.as_deref(), Some("AIzaSWKEY0123456789"));
        assert_eq!(transport.call_count(), 1);
    }

    #[tokio::test]
    async fn session_is_cached_between_calls() {
        let sw = json!([[{"visitorData": "VIS"}], "AIzaSWKEY0123456789"]);
        let transport = Arc::new(ReplayTransport::json(Bytes::from(sw.to_string())));
        let client = client(transport.clone());
        let _ = client.session(&ctx()).await;
        let _ = client.session(&ctx()).await;
        assert_eq!(transport.call_count(), 1, "session must not re-bootstrap");
    }

    #[tokio::test]
    async fn session_falls_back_to_profile_api_key() {
        let transport = Arc::new(ReplayTransport::new(vec![]));
        let client = client(transport);
        let session = client.session(&ctx()).await;
        assert_eq!(session.api_key.as_deref(), Some("AIzaTESTKEY"));
        assert_eq!(session.visitor_data, None);
    }

    #[tokio::test]
    async fn http_errors_are_classified() {
        for (status, expected) in [
            (429u16, "provider_rate_limited"),
            (403, "provider_unauthorized"),
            (500, "provider_upstream"),
        ] {
            let transport = Arc::new(ReplayTransport::new(vec![
                // session bootstrap succeeds
                Ok(crate::TransportResponse {
                    status: 200,
                    headers: vec![],
                    body: Bytes::from(
                        json!([[{"visitorData": "V"}], "AIzaSWKEY0123456789"]).to_string(),
                    ),
                    elapsed: Duration::from_millis(1),
                }),
                // then the endpoint fails with the status under test
                Ok(crate::TransportResponse {
                    status,
                    headers: vec![("retry-after".into(), "2".into())],
                    body: Bytes::from_static(b"{\"error\":\"nope\"}"),
                    elapsed: Duration::from_millis(3),
                }),
            ]));
            let client = client(transport);
            let error = client
                .post_json("search", json!({}), &ctx())
                .await
                .expect_err("must fail");
            assert_eq!(error.code(), expected, "status {status}");
        }
    }

    #[tokio::test]
    async fn expired_deadline_fails_before_sending() {
        let transport = Arc::new(ReplayTransport::new(vec![]));
        let client = client(transport.clone());
        let mut expired = ctx();
        expired.deadline = Instant::now() - Duration::from_millis(1);
        let error = client
            .post_json("search", json!({}), &expired)
            .await
            .expect_err("must fail");
        assert_eq!(error, ProviderError::Timeout);
        assert_eq!(transport.call_count(), 0);
    }

    #[tokio::test]
    async fn unauthorised_invalidates_the_session() {
        let sw = json!([[{"visitorData": "VIS"}], "AIzaSWKEY0123456789"]);
        let transport = Arc::new(ReplayTransport::new(vec![
            Ok(crate::TransportResponse {
                status: 200,
                headers: vec![],
                body: Bytes::from(sw.to_string()),
                elapsed: Duration::from_millis(1),
            }),
            Ok(crate::TransportResponse {
                status: 403,
                headers: vec![],
                body: Bytes::new(),
                elapsed: Duration::from_millis(1),
            }),
        ]));
        let client = client(transport);
        let _ = client.session(&ctx()).await;
        let _ = client.post_json("player", json!({}), &ctx()).await;
        assert_eq!(client.session.lock().await.as_ref(), None);
    }
}
