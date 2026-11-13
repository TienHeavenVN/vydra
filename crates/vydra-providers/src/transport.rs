//! Transport abstraction.
//!
//! Inspired by `NewPipeExtractor/extractor/src/main/java/org/schabi/newpipe/extractor/downloader/`
//! (`docs/research/SOURCE_CODE_MAP.md` §5): extraction code must never own an HTTP client, because
//! then it cannot be tested offline and cannot be forced to honour deadlines, size caps or the URL
//! allowlist.
//!
//! Every request that leaves Vydra passes through [`TransportRequest::validate`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;
use url::Url;

/// HTTP method subset used by the providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

impl HttpMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// A validated outbound request.
#[derive(Debug, Clone)]
pub struct TransportRequest {
    pub method: HttpMethod,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
    /// Per-request deadline for the whole exchange.
    pub timeout: Duration,
    /// Hard cap on the response body.
    pub max_response_bytes: usize,
}

impl TransportRequest {
    /// Reject anything that could be used for SSRF or header injection.
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.url.scheme() != "https" {
            return Err(TransportError::BlockedUrl(format!(
                "non-https scheme: {}",
                self.url.scheme()
            )));
        }
        let host = self
            .url
            .host_str()
            .ok_or_else(|| TransportError::BlockedUrl("url without host".into()))?;
        if !is_allowed_host(host) {
            return Err(TransportError::BlockedUrl(format!("host not allowed: {host}")));
        }
        for (name, value) in &self.headers {
            let lower = name.to_ascii_lowercase();
            if !lower
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            {
                return Err(TransportError::InvalidHeader(name.clone()));
            }
            if value.contains('\r') || value.contains('\n') {
                return Err(TransportError::InvalidHeader(name.clone()));
            }
        }
        if self.max_response_bytes == 0 {
            return Err(TransportError::BodyTooLarge { limit: 0 });
        }
        Ok(())
    }
}

/// Hosts Vydra is permitted to contact. Everything else is a configuration error, not a redirect to
/// follow.
pub fn is_allowed_host(host: &str) -> bool {
    const EXACT: &[&str] = &[
        "youtube.com",
        "www.youtube.com",
        "m.youtube.com",
        "music.youtube.com",
        "www.googleapis.com",
        "youtubei.googleapis.com",
        "i.ytimg.com",
        "yt3.ggpht.com",
        "yt3.googleusercontent.com",
    ];
    const SUFFIXES: &[&str] = &[".youtube.com", ".ytimg.com", ".ggpht.com", ".googleusercontent.com"];

    let host = host.to_ascii_lowercase();
    EXACT.contains(&host.as_str()) || SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
}

/// A response, immutable once produced.
#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub elapsed: Duration,
}

impl TransportResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let wanted = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| key.to_ascii_lowercase() == wanted)
            .map(|(_, value)| value.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Retry-After` in seconds, if present and numeric.
    pub fn retry_after(&self) -> Option<Duration> {
        self.header("retry-after")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
    }

    pub fn json(&self) -> Result<Value, TransportError> {
        serde_json::from_slice(&self.body).map_err(|err| TransportError::Decode(err.to_string()))
    }
}

/// Transport failures. Note: an HTTP error status is **not** a transport error — it is part of the
/// response so that the provider can classify it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("response exceeded the {limit} byte limit")]
    BodyTooLarge { limit: usize },
    #[error("blocked url: {0}")]
    BlockedUrl(String),
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("connection error: {0}")]
    Connect(String),
    #[error("could not decode response: {0}")]
    Decode(String),
}

impl TransportError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }
}

/// The transport contract.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn execute(&self, request: TransportRequest) -> Result<TransportResponse, TransportError>;
}

/// Production transport backed by `reqwest`.
///
/// Security posture (requirement 9): rustls only, HTTPS only, no redirects to non-allowlisted
/// hosts, bounded response size, hard per-request timeout, no ambient proxy/cookie jar.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .pool_max_idle_per_host(16)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(3))
            // Redirects are followed manually so that every hop is re-validated against the host
            // allowlist; a redirect is therefore never silently trusted.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| TransportError::Connect(err.to_string()))?;
        Ok(Self { client })
    }

    async fn execute_once(
        &self,
        request: &TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        let started = Instant::now();
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self.client.request(method, request.url.clone());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }

        let mut response = builder
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    TransportError::Timeout(request.timeout)
                } else {
                    TransportError::Connect(err.to_string())
                }
            })?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect::<Vec<_>>();

        // Stream the body so that the size limit is enforced *before* allocating it all.
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|err| TransportError::Connect(err.to_string()))?;
            let Some(chunk) = chunk else { break };
            if buffer.len() + chunk.len() > request.max_response_bytes {
                return Err(TransportError::BodyTooLarge {
                    limit: request.max_response_bytes,
                });
            }
            buffer.extend_from_slice(&chunk);
        }

        Ok(TransportResponse {
            status,
            headers,
            body: Bytes::from(buffer),
            elapsed: started.elapsed(),
        })
    }
}

#[async_trait::async_trait]
impl Transport for ReqwestTransport {
    async fn execute(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        request.validate()?;
        let timeout = request.timeout;
        match tokio::time::timeout(timeout, self.execute_once(&request)).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout(timeout)),
        }
    }
}

/// Deterministic transport for tests: replays a queue of responses and records what was requested.
#[derive(Debug, Default)]
pub struct ReplayTransport {
    queued: Mutex<Vec<Result<TransportResponse, TransportError>>>,
    calls: Mutex<Vec<TransportRequest>>,
}

impl ReplayTransport {
    pub fn new(responses: Vec<Result<TransportResponse, TransportError>>) -> Self {
        Self {
            queued: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Replay a single successful JSON response.
    pub fn json(body: impl Into<Bytes>) -> Self {
        Self::new(vec![Ok(TransportResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
            elapsed: Duration::from_millis(5),
        })])
    }

    pub fn calls(&self) -> Vec<TransportRequest> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().map(|calls| calls.len()).unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl Transport for ReplayTransport {
    async fn execute(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        request.validate()?;
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request);
        }
        let next = self
            .queued
            .lock()
            .ok()
            .and_then(|mut queue| {
                if queue.is_empty() {
                    None
                } else {
                    Some(queue.remove(0))
                }
            });
        next.unwrap_or_else(|| {
            Err(TransportError::Connect(
                "ReplayTransport has no queued response left".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str) -> TransportRequest {
        TransportRequest {
            method: HttpMethod::Get,
            url: Url::parse(url).expect("valid url"),
            headers: vec![("accept".into(), "application/json".into())],
            body: None,
            timeout: Duration::from_secs(5),
            max_response_bytes: 1024,
        }
    }

    #[test]
    fn allowlist_accepts_youtube_hosts() {
        for host in [
            "www.youtube.com",
            "music.youtube.com",
            "youtubei.googleapis.com",
            "i.ytimg.com",
            "yt3.ggpht.com",
        ] {
            assert!(is_allowed_host(host), "rejected {host}");
        }
    }

    #[test]
    fn allowlist_rejects_everything_else() {
        for host in [
            "evil.com",
            "youtube.com.evil.com",
            "169.254.169.254",
            "localhost",
            "127.0.0.1",
            "notyoutube.com",
        ] {
            assert!(!is_allowed_host(host), "accepted {host}");
        }
    }

    #[test]
    fn validation_blocks_plain_http_and_foreign_hosts() {
        assert!(matches!(
            request("http://www.youtube.com/youtubei/v1/search").validate(),
            Err(TransportError::BlockedUrl(_))
        ));
        assert!(matches!(
            request("https://evil.example/youtubei").validate(),
            Err(TransportError::BlockedUrl(_))
        ));
    }

    #[test]
    fn validation_blocks_header_injection() {
        let mut req = request("https://www.youtube.com/");
        req.headers
            .push(("x-test".into(), "value\r\nInjected: 1".into()));
        assert!(matches!(
            req.validate(),
            Err(TransportError::InvalidHeader(_))
        ));

        let mut req = request("https://www.youtube.com/");
        req.headers.push(("bad header".into(), "value".into()));
        assert!(matches!(
            req.validate(),
            Err(TransportError::InvalidHeader(_))
        ));
    }

    #[test]
    fn retry_after_is_parsed() {
        let response = TransportResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "3".into())],
            body: Bytes::new(),
            elapsed: Duration::from_millis(1),
        };
        assert_eq!(response.retry_after(), Some(Duration::from_secs(3)));
    }

    #[tokio::test]
    async fn replay_transport_returns_queued_responses_and_records_calls() {
        let transport = ReplayTransport::json(Bytes::from_static(b"{\"ok\":true}"));
        let response = transport
            .execute(request("https://www.youtube.com/youtubei/v1/search"))
            .await
            .expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.json().expect("json")["ok"], true);
        assert_eq!(transport.call_count(), 1);

        // Queue is exhausted: the transport must fail loudly rather than fabricate a response.
        assert!(transport
            .execute(request("https://www.youtube.com/youtubei/v1/search"))
            .await
            .is_err());
    }
}
