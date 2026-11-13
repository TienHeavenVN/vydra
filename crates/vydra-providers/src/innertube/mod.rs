//! InnerTube provider: session bootstrap, request composition, parsing and mapping.

pub mod client;
pub mod map;
pub mod params;
pub mod provider;

use std::time::Duration;

pub use client::{InnertubeClient, SessionData};
pub use provider::InnertubeProvider;

use crate::ProviderId;

/// InnerTube API root (observed in every reference implementation, see
/// `docs/research/SOURCE_CODE_MAP.md` §1 — MediaServiceCore `URLS.API.PRODUCTION_1`).
pub const INNERTUBE_BASE: &str = "https://www.youtube.com/youtubei/v1";

/// Session bootstrap endpoint. Used by MediaServiceCore (`SessionApi.getSessionData`) and
/// YouTube.js (`Session.getSessionData`).
pub const SW_JS_DATA_URL: &str = "https://www.youtube.com/sw.js_data";

/// HTML fallback used when `sw.js_data` does not answer.
pub const YOUTUBE_HOME: &str = "https://www.youtube.com/";

// NOTE (audit 2026-11-13): the reference projects hardcode a public YouTube web API key.
// Vydra deliberately does **not** ship one:
//   * a shared key literal is a credential-hygiene problem and trips secret scanning,
//   * it silently ties every deployment to one quota identity,
//   * its presence cannot be verified from here, so shipping it would imply a guarantee we cannot make.
// The key is therefore obtained at runtime (session bootstrap) or supplied by the operator via
// `VYDRA__PROVIDERS__INNERTUBE__API_KEY`. Absence of a key is logged, not fatal
// (see docs/security/PROVIDER_ENGINE_AUDIT.md, finding A-01).

/// InnerTube client family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Web,
    MWeb,
    Tv,
}

impl ClientKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "web" => Some(Self::Web),
            "mweb" => Some(Self::MWeb),
            "tv" | "tvhtml5" => Some(Self::Tv),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::MWeb => "mweb",
            Self::Tv => "tv",
        }
    }
}

/// A client profile: the exact identity Vydra presents upstream.
///
/// Versions are data, not code, so an operator can rotate one without a release (ADR-004 and
/// `docs/research/EXTRACTION_METHODS.md` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProfile {
    pub kind: ClientKind,
    /// `clientName` in the request context.
    pub name: &'static str,
    /// Numeric proto id for the `X-Youtube-Client-Name` header.
    pub name_id: &'static str,
    pub version: String,
    pub user_agent: Option<String>,
    pub platform: Option<&'static str>,
    pub api_key: Option<String>,
}

impl ClientProfile {
    /// WEB profile.
    ///
    /// Default version copied from `NewPipeExtractor/.../youtube/ClientsConstants.java`
    /// (`WEB_HARDCODED_CLIENT_VERSION`), which is documented upstream as a *fallback* value — the
    /// real version is normally read from `ytcfg`. Vydra prefers the bootstrapped version and only
    /// falls back to this literal.
    pub fn web(version_override: Option<&str>) -> Self {
        Self {
            kind: ClientKind::Web,
            name: "WEB",
            name_id: "1",
            version: version_override.unwrap_or("2.20260805.01.00").to_owned(),
            user_agent: Some(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/141.0.0.0 Safari/537.36"
                    .to_owned(),
            ),
            platform: Some("DESKTOP"),
            api_key: None,
        }
    }

    /// MWEB profile (mobile web).
    pub fn mweb(version_override: Option<&str>) -> Self {
        Self {
            kind: ClientKind::MWeb,
            name: "MWEB",
            name_id: "2",
            version: version_override.unwrap_or("2.20260805.00.00").to_owned(),
            user_agent: Some(
                "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like \
                 Gecko) Version/17.5 Mobile/15E148 Safari/604.1,gzip(gfe)"
                    .to_owned(),
            ),
            platform: None,
            api_key: None,
        }
    }

    /// TVHTML5 profile. Kept for capability experiments; not used by default.
    pub fn tv(version_override: Option<&str>) -> Self {
        Self {
            kind: ClientKind::Tv,
            name: "TVHTML5",
            name_id: "7",
            version: version_override.unwrap_or("7.20250101.10.00").to_owned(),
            user_agent: Some("Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version".to_owned()),
            platform: None,
            api_key: None,
        }
    }

    pub fn from_kind(kind: ClientKind, version_override: Option<&str>) -> Self {
        match kind {
            ClientKind::Web => Self::web(version_override),
            ClientKind::MWeb => Self::mweb(version_override),
            ClientKind::Tv => Self::tv(version_override),
        }
    }

    /// Short label used in provenance and metrics.
    pub fn label(&self) -> String {
        format!("{}-{}", self.name.to_ascii_lowercase(), self.version)
    }
}

/// Provider-level configuration (validated by `vydra-config` at startup).
#[derive(Debug, Clone)]
pub struct InnertubeProviderConfig {
    pub provider_id: ProviderId,
    pub client: ClientProfile,
    pub hl: String,
    pub gl: String,
    pub request_timeout: Duration,
    pub session_ttl: Duration,
    pub max_response_bytes: usize,
    pub consent_cookie: Option<String>,
    pub api_key_override: Option<String>,
    /// Enables `SEARCH_FILTERS` params encoding, which is **not verified against the live API**
    /// (see `params.rs`). Off by default so the capability is not advertised dishonestly.
    pub enable_unverified_filters: bool,
}

impl Default for InnertubeProviderConfig {
    fn default() -> Self {
        Self {
            provider_id: ProviderId::new("innertube-web"),
            client: ClientProfile::web(None),
            hl: "en".to_owned(),
            gl: "US".to_owned(),
            request_timeout: Duration::from_millis(6_000),
            session_ttl: Duration::from_secs(6 * 60 * 60),
            max_response_bytes: 4 * 1024 * 1024,
            consent_cookie: None,
            api_key_override: None,
            enable_unverified_filters: false,
        }
    }
}

impl InnertubeProviderConfig {
    pub fn with_client(mut self, kind: ClientKind) -> Self {
        self.client = ClientProfile::from_kind(kind, None);
        self.provider_id = ProviderId::new(match kind {
            ClientKind::Web => "innertube-web",
            ClientKind::MWeb => "innertube-mweb",
            ClientKind::Tv => "innertube-tv",
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_profile_ids_match_reference_constants() {
        // Numeric ids cross-checked against NewPipeExtractor's ClientsConstants.java.
        assert_eq!(ClientProfile::web(None).name_id, "1");
        assert_eq!(ClientProfile::mweb(None).name_id, "2");
        assert_eq!(ClientProfile::tv(None).name_id, "7");
    }

    #[test]
    fn version_override_is_honoured() {
        let profile = ClientProfile::web(Some("9.9.9"));
        assert_eq!(profile.version, "9.9.9");
        assert_eq!(profile.label(), "web-9.9.9");
    }

    #[test]
    fn client_kind_parsing_is_strict() {
        assert_eq!(ClientKind::parse("WEB"), Some(ClientKind::Web));
        assert_eq!(ClientKind::parse("mweb"), Some(ClientKind::MWeb));
        assert_eq!(ClientKind::parse("tvhtml5"), Some(ClientKind::Tv));
        assert_eq!(ClientKind::parse("android"), None);
    }
}
