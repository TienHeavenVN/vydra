//! Transport abstraction.
//!
//! Inspired by `NewPipeExtractor/extractor/src/main/java/org/schabi/newpipe/extractor/downloader/`
//! (`docs/research/SOURCE_CODE_MAP.md` §5): extraction code must never own an HTTP client, because
//! then it cannot be tested offline and cannot be forced to honour deadlines, size caps or the URL
//! allowlist.
//!
//! Every request that leaves Vydra passes through [`TransportRequest::validate`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
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
    ///
    /// Defence in depth: scheme, userinfo, port, fragment, host allowlist, header shape, header
    /// count and body limits are all checked *before* any DNS resolution happens. See
    /// `docs/security/PROVIDER_ENGINE_AUDIT.md` §3 for the residual risks (notably DNS rebinding,
    /// which a hostname allowlist alone cannot eliminate).
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.url.scheme() != "https" {
            return Err(TransportError::BlockedUrl(format!(
                "non-https scheme: {}",
                self.url.scheme()
            )));
        }
        // `https://user:pass@host/` is never legitimate here and is a classic parser-confusion
        // vector (`https://www.youtube.com@evil.example/`).
        if !self.url.username().is_empty() || self.url.password().is_some() {
            return Err(TransportError::BlockedUrl(
                "url must not contain userinfo".into(),
            ));
        }
        // Only the default HTTPS port is allowed: a non-standard port is either a mistake or an
        // attempt to reach an internal service through an allowlisted hostname.
        match self.url.port() {
            None | Some(443) => {}
            Some(port) => {
                return Err(TransportError::BlockedUrl(format!(
                    "non-default port not allowed: {port}"
                )));
            }
        }
        if self.url.fragment().is_some() {
            return Err(TransportError::BlockedUrl(
                "url must not contain a fragment".into(),
            ));
        }
        let host = self
            .url
            .host_str()
            .ok_or_else(|| TransportError::BlockedUrl("url without host".into()))?;
        if !is_allowed_host(host) {
            return Err(TransportError::BlockedUrl(format!(
                "host not allowed: {host}"
            )));
        }
        if self.headers.len() > MAX_HEADERS {
            return Err(TransportError::InvalidHeader(format!(
                "too many headers ({})",
                self.headers.len()
            )));
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
        if let Some(body) = &self.body
            && body.len() > MAX_REQUEST_BODY_BYTES
        {
            return Err(TransportError::BodyTooLarge {
                limit: MAX_REQUEST_BODY_BYTES,
            });
        }
        Ok(())
    }
}

/// Maximum number of outbound headers (defensive bound).
const MAX_HEADERS: usize = 32;
/// Maximum outbound request body accepted by the transport.
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;

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
    const SUFFIXES: &[&str] = &[
        ".youtube.com",
        ".ytimg.com",
        ".ggpht.com",
        ".googleusercontent.com",
    ];

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
    async fn execute(&self, request: TransportRequest)
    -> Result<TransportResponse, TransportError>;
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
            // A.30: `reqwest` honours `HTTPS_PROXY` by default, and a proxy resolves the target
            // itself — which would silently disable the resolver guard below and let the proxy reach
            // an internal address on Vydra's behalf. Proxies are refused, not inherited from the
            // environment; a deployment that needs one must say so explicitly (and then the guard no
            // longer covers the hop, see docs/security/DNS_REBINDING_ANALYSIS.md §5).
            .no_proxy()
            // Redirects are **refused**, not followed. A 3xx is returned to the caller as an
            // ordinary non-success response, so there is no hop that could bypass the allowlist
            // and no code path that silently trusts a `Location` header.
            .redirect(reqwest::redirect::Policy::none())
            // Resolve, validate, and connect to the *same* addresses: a hostname allowlist on its
            // own is defeated by a name that resolves to an internal address (DNS rebinding).
            .dns_resolver(Arc::new(GuardedResolver::new()))
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

        let mut response = builder.send().await.map_err(|err| {
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
    async fn execute(
        &self,
        request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        request.validate()?;
        let timeout = request.timeout;
        match tokio::time::timeout(timeout, self.execute_once(&request)).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout(timeout)),
        }
    }
}

/// Address families Vydra refuses to connect to, regardless of which hostname resolved there.
///
/// The hostname allowlist answers "may we talk to this name?"; this answers "may we connect to this
/// address?". Both are required: `youtube.com` resolving to `127.0.0.1` or `169.254.169.254` must
/// fail, and a rebinding resolver can produce exactly that after the name has been approved.
pub fn prohibited_ip_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => prohibited_v4(v4),
        IpAddr::V6(v6) => prohibited_v6(v6),
    }
}

fn prohibited_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let octets = ip.octets();
    if ip.is_unspecified() || octets[0] == 0 {
        return Some("unspecified");
    }
    if ip.is_loopback() {
        return Some("loopback");
    }
    if ip.is_private() {
        return Some("private");
    }
    if ip.is_link_local() {
        return Some("link-local");
    }
    if ip.is_broadcast() {
        return Some("broadcast");
    }
    if ip.is_multicast() {
        return Some("multicast");
    }
    // Carrier-grade NAT (RFC 6598): never a legitimate public endpoint.
    if octets[0] == 100 && (64..128).contains(&octets[1]) {
        return Some("cgnat");
    }
    // Documentation ranges (RFC 5737): must never appear in real DNS answers.
    if (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
    {
        return Some("documentation");
    }
    // Benchmarking (RFC 2544) and reserved space (RFC 1112).
    if (octets[0] == 198 && (18..20).contains(&octets[1])) || octets[0] >= 240 {
        return Some("reserved");
    }
    None
}

fn prohibited_v6(ip: Ipv6Addr) -> Option<&'static str> {
    if ip.is_unspecified() {
        return Some("unspecified");
    }
    if ip.is_loopback() {
        return Some("loopback");
    }
    if ip.is_multicast() {
        return Some("multicast");
    }
    // An address that wraps IPv4 is judged as that IPv4 address: `::ffff:127.0.0.1` is loopback.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return prohibited_v4(v4).or(Some("ipv4-mapped"));
    }
    let segments = ip.segments();
    // Deprecated IPv4-compatible form (::a.b.c.d): first 96 bits zero, last 32 bits an address.
    if segments[0..6].iter().all(|segment| *segment == 0) && (segments[6] != 0 || segments[7] != 0)
    {
        let embedded = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        );
        return prohibited_v4(embedded).or(Some("ipv4-compatible"));
    }
    if segments[0] & 0xfe00 == 0xfc00 {
        return Some("unique-local");
    }
    if segments[0] & 0xffc0 == 0xfe80 {
        return Some("link-local");
    }
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return Some("documentation");
    }
    // 6to4 (2002::/16) embeds an IPv4 address in segments 1-2: judge the embedded address.
    if segments[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        );
        return prohibited_v4(embedded).or(Some("6to4"));
    }
    // Teredo (2001::/32) and the discard prefix (100::/64) are tunnel/blackhole space.
    if segments[0] == 0x2001 && segments[1] == 0 {
        return Some("teredo");
    }
    if segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0 {
        return Some("discard");
    }
    None
}

/// Keep only addresses that may be connected to, and refuse to continue if none survive.
///
/// Returning "some of the addresses were fine" is what pins the connection to a verified
/// resolution: the addresses dropped here cannot be re-offered by a second lookup, because the
/// resolver below is the only lookup this client performs.
pub fn guard_resolution<I>(addrs: I) -> Result<Vec<SocketAddr>, TransportError>
where
    I: IntoIterator<Item = SocketAddr>,
{
    let mut permitted: Vec<SocketAddr> = Vec::new();
    let mut blocked: u32 = 0;
    for addr in addrs {
        match prohibited_ip_reason(addr.ip()) {
            None => permitted.push(addr),
            Some(reason) => {
                blocked = blocked.saturating_add(1);
                tracing::warn!(ip = %addr.ip(), reason, "blocked resolved address");
            }
        }
    }
    if permitted.is_empty() {
        return Err(TransportError::BlockedUrl(format!(
            "resolution yielded no permitted address ({blocked} blocked)"
        )));
    }
    permitted.sort();
    permitted.dedup();
    Ok(permitted)
}

/// Resolver that validates every address before the socket is opened.
///
/// `reqwest` calls this once per connection attempt and connects to the returned addresses
/// directly, so there is no window in which the name can resolve differently between validation
/// and connection. TLS hostname verification is unaffected: the URL still carries the hostname, and
/// the certificate is still checked against it.
#[derive(Debug, Default, Clone, Copy)]
pub struct GuardedResolver;

impl GuardedResolver {
    pub const fn new() -> Self {
        Self
    }
}

fn boxed_error(err: TransportError) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(err)
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        Box::pin(async move {
            let hostname = host.trim_end_matches('.').to_owned();
            if !is_allowed_host(&hostname) {
                return Err(boxed_error(TransportError::BlockedUrl(format!(
                    "resolver refused host: {hostname}"
                ))));
            }
            let resolved = tokio::net::lookup_host((hostname.as_str(), 443u16))
                .await
                .map_err(|err| {
                    boxed_error(TransportError::Connect(format!(
                        "dns resolution failed for {hostname}: {err}"
                    )))
                })?;
            let guarded = guard_resolution(resolved).map_err(boxed_error)?;
            Ok(Box::new(guarded.into_iter()) as Addrs)
        })
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
    async fn execute(
        &self,
        request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        request.validate()?;
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request);
        }
        let next = self.queued.lock().ok().and_then(|mut queue| {
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
    fn prohibited_ipv4_ranges_are_rejected() {
        for text in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.5.5",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.5",
            "198.51.100.7",
            "203.0.113.9",
            "198.18.0.1",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
        ] {
            let ip: IpAddr = text.parse().expect("ip");
            assert!(
                prohibited_ip_reason(ip).is_some(),
                "should have been prohibited: {text}"
            );
        }
    }

    #[test]
    fn public_ipv4_addresses_are_permitted() {
        for text in ["142.250.72.14", "8.8.8.8", "1.1.1.1", "185.199.108.153"] {
            let ip: IpAddr = text.parse().expect("ip");
            assert_eq!(prohibited_ip_reason(ip), None, "rejected {text}");
        }
    }

    #[test]
    fn prohibited_ipv6_ranges_are_rejected() {
        for text in [
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "2001:db8::1",
            "2001:0:53aa:64c::1",
            "100::1",
            // IPv4-mapped: judged as the wrapped IPv4 address.
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            // Deprecated IPv4-compatible form.
            "::127.0.0.1",
            // 6to4 wrapping loopback.
            "2002:7f00:1::1",
        ] {
            let ip: IpAddr = text.parse().expect("ip");
            assert!(
                prohibited_ip_reason(ip).is_some(),
                "should have been prohibited: {text}"
            );
        }
    }

    #[test]
    fn public_ipv6_addresses_are_permitted() {
        for text in ["2607:f8b0:4005:80b::200e", "2001:4860:4860::8888"] {
            let ip: IpAddr = text.parse().expect("ip");
            assert_eq!(prohibited_ip_reason(ip), None, "rejected {text}");
        }
    }

    #[test]
    fn guard_resolution_drops_blocked_addresses_and_keeps_order_independence() {
        let google = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(142, 250, 72, 14)), 443);
        let mixed = vec![
            google,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 443),
            google,
        ];
        let guarded = guard_resolution(mixed).expect("one address survives");
        assert_eq!(guarded, vec![google]);
    }

    #[test]
    fn guard_resolution_fails_closed_when_every_address_is_blocked() {
        let blocked = vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443),
        ];
        assert!(matches!(
            guard_resolution(blocked),
            Err(TransportError::BlockedUrl(_))
        ));
    }

    #[tokio::test]
    async fn resolver_refuses_a_host_that_is_not_allowlisted_without_resolving_it() {
        let resolver = GuardedResolver::new();
        let name: reqwest::dns::Name = "rebind.example".parse().expect("name");
        let result = resolver.resolve(name).await;
        assert!(result.is_err(), "non-allowlisted host resolved");
    }

    #[tokio::test]
    async fn resolver_blocks_a_hostname_that_resolves_to_loopback() {
        // `localhost` is not allowlisted, so the host gate must reject it before any lookup; this
        // asserts the gate, not the system resolver.
        let resolver = GuardedResolver::new();
        let name: reqwest::dns::Name = "localhost".parse().expect("name");
        assert!(resolver.resolve(name).await.is_err());
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
    fn rejects_ip_literals_and_userinfo() {
        // IP literals must never be allowlisted: `is_allowed_host` compares names only, so a
        // numeric host has to fail here.
        for bad in [
            "https://127.0.0.1/youtubei/v1/search",
            "https://[::1]/youtubei/v1/search",
            "https://169.254.169.254/latest/meta-data",
            "https://www.youtube.com@evil.example/x",
            "https://user:pass@www.youtube.com/x",
        ] {
            let result = request(bad).validate();
            assert!(
                matches!(result, Err(TransportError::BlockedUrl(_))),
                "accepted {bad}: {result:?}"
            );
        }
    }

    #[test]
    fn rejects_non_default_ports_and_fragments() {
        assert!(matches!(
            request("https://www.youtube.com:8443/x").validate(),
            Err(TransportError::BlockedUrl(_)),
        ));
        assert!(matches!(
            request("https://www.youtube.com/x#frag").validate(),
            Err(TransportError::BlockedUrl(_)),
        ));
        assert!(request("https://www.youtube.com:443/x").validate().is_ok());
    }

    #[test]
    fn rejects_other_schemes() {
        for bad in ["http://www.youtube.com/x", "ftp://www.youtube.com/x"] {
            assert!(matches!(
                request(bad).validate(),
                Err(TransportError::BlockedUrl(_)),
            ));
        }
    }

    #[test]
    fn rejects_oversized_request_body_and_header_count() {
        let mut req = request("https://www.youtube.com/x");
        req.body = Some(Bytes::from(vec![b'x'; MAX_REQUEST_BODY_BYTES + 1]));
        assert!(matches!(
            req.validate(),
            Err(TransportError::BodyTooLarge { .. })
        ));

        let mut req = request("https://www.youtube.com/x");
        for index in 0..(MAX_HEADERS + 1) {
            req.headers.push((format!("x-h{index}"), "v".into()));
        }
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
        assert!(
            transport
                .execute(request("https://www.youtube.com/youtubei/v1/search"))
                .await
                .is_err()
        );
    }
}
