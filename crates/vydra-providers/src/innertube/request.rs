//! Typed request bodies for the verified InnerTube endpoints.
//!
//! Phase 4.2 pinned the *shape* of what Vydra sends with string-level assertions in
//! `tests/request_contract.rs`. Those assertions could only catch a change after the fact. This module
//! makes the shapes structural instead:
//!
//! * a continuation request **cannot** carry a `query` — the type has no such field;
//! * `params` exists only on [`SearchRequest`], is `None` unless a caller explicitly passes verified
//!   bytes, and is skipped when serializing (so the wire format is `{context, query}`);
//! * field order is the struct order, so serialization is byte-deterministic (same request → same
//!   body), which is what makes an exact-string contract test meaningful;
//! * the `context` object is built once from the client profile and the session, instead of being
//!   assembled with `json!` in each call site.
//!
//! Citations for every field are in `docs/research/INNERTUBE_REQUEST_CONTRACTS.md` §3.

use serde::{Deserialize, Serialize};

use super::ClientProfile;
use super::client::SessionData;

/// The `client` member of the InnerTube context.
///
/// `clientName` is the profile name (`WEB`), not the numeric header value — the header carries
/// `x-youtube-client-name: 1` while the body carries `"clientName": "WEB"`. Both are verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub hl: String,
    pub gl: String,
    pub client_name: String,
    pub client_version: String,
    pub original_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visitor_data: Option<String>,
}

/// `context.user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserContext {
    pub locked_safety_mode: bool,
}

/// `context.request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub use_ssl: bool,
    #[serde(default)]
    pub internal_experiment_flags: Vec<String>,
}

/// The shared `context` object.
///
/// Member order is `client`, `user`, `request` — matching every reference implementation and the
/// recorded request shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientContext {
    pub client: ClientInfo,
    pub user: UserContext,
    pub request: RequestContext,
}

impl ClientContext {
    /// Build the context for a client profile and a session.
    ///
    /// `visitor_data` is omitted when the bootstrap produced none: an empty string is not the same
    /// thing as an absent value, and only one of them is the documented shape.
    pub fn new(profile: &ClientProfile, session: &SessionData, hl: &str, gl: &str) -> Self {
        let client_version = session
            .client_version
            .clone()
            .unwrap_or_else(|| profile.version.clone());
        Self {
            client: ClientInfo {
                hl: hl.to_owned(),
                gl: gl.to_owned(),
                client_name: profile.name.to_owned(),
                client_version,
                original_url: "https://www.youtube.com".to_owned(),
                platform: profile.platform.map(str::to_owned),
                visitor_data: session
                    .visitor_data
                    .clone()
                    .filter(|value| !value.is_empty()),
            },
            user: UserContext {
                locked_safety_mode: false,
            },
            request: RequestContext {
                use_ssl: true,
                internal_experiment_flags: Vec::new(),
            },
        }
    }
}

/// First-page search body: `{context, query[, params]}`.
///
/// `params` is the protobuf-encoded filter/sort blob. Its encoding is **not verified against the live
/// API** (`params.rs`), so it is only ever set by the gated filter path and never by default; when it
/// is `None` the field is absent from the wire format rather than sent as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub context: ClientContext,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<String>,
}

impl SearchRequest {
    /// First page without filters — the only search Vydra performs unless the unverified filter
    /// capability is explicitly enabled.
    pub fn new(context: ClientContext, query: impl Into<String>) -> Self {
        Self {
            context,
            query: query.into(),
            params: None,
        }
    }

    /// First page with a pre-encoded `params` blob.
    ///
    /// The caller is responsible for the capability gate: `params` encoding is unverified, so this
    /// must only be reachable when `enable_unverified_filters` is set. It is public because the
    /// params builder lives next door, not because it is safe to call from anywhere.
    pub fn with_params(
        context: ClientContext,
        query: impl Into<String>,
        params: impl Into<String>,
    ) -> Self {
        Self {
            context,
            query: query.into(),
            params: Some(params.into()),
        }
    }

    /// The encoded filter blob, if any.
    pub fn params(&self) -> Option<&str> {
        self.params.as_deref()
    }

    /// Whether this request carries an unverified `params` blob.
    pub fn has_params(&self) -> bool {
        self.params.is_some()
    }
}

/// Continuation body: `{context, continuation}`.
///
/// There is deliberately no `query`, no `params` and no `cursor` field: a continuation page carries
/// the token and nothing else. Reference evidence is in
/// `docs/research/INNERTUBE_REQUEST_CONTRACTS.md` §4, and the Phase 4.3 probe confirmed it against
/// the live endpoint (token-only body → 200, complete second page). The type is what prevents the
/// mistake, not review attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationRequest {
    pub context: ClientContext,
    pub continuation: String,
}

impl ContinuationRequest {
    pub fn new(context: ClientContext, continuation: impl Into<String>) -> Self {
        Self {
            context,
            continuation: continuation.into(),
        }
    }
}

/// Player body: `{context, videoId, contentCheckOk, racyCheckOk}`.
///
/// Kept here so the context object has exactly one source; the flags are request-side only and are
/// what the web client sends so that age-restricted *metadata* is not filtered out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerRequest {
    pub context: ClientContext,
    pub video_id: String,
    pub content_check_ok: bool,
    pub racy_check_ok: bool,
}

impl PlayerRequest {
    pub fn new(context: ClientContext, video_id: impl Into<String>) -> Self {
        Self {
            context,
            video_id: video_id.into(),
            content_check_ok: true,
            racy_check_ok: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ClientProfile {
        ClientProfile::web(None)
    }

    fn session() -> SessionData {
        SessionData {
            api_key: Some("AIzaKEY".to_owned()),
            visitor_data: Some("VISITOR".to_owned()),
            client_version: Some("2.TEST".to_owned()),
            fetched_at: None,
        }
    }

    fn context() -> ClientContext {
        ClientContext::new(&profile(), &session(), "en", "US")
    }

    #[test]
    fn context_serializes_to_the_documented_object() {
        let json = serde_json::to_string(&context()).expect("serialize");
        assert_eq!(
            json,
            concat!(
                r#"{"client":{"hl":"en","gl":"US","clientName":"WEB","clientVersion":"2.TEST","#,
                r#""originalUrl":"https://www.youtube.com","platform":"DESKTOP","#,
                r#""visitorData":"VISITOR"},"#,
                r#""user":{"lockedSafetyMode":false},"#,
                r#""request":{"useSsl":true,"internalExperimentFlags":[]}}"#
            ),
            "field order is part of the contract: it makes serialization deterministic"
        );
    }

    #[test]
    fn context_omits_visitor_data_when_the_session_has_none() {
        let empty = SessionData::default();
        let context = ClientContext::new(&profile(), &empty, "de", "DE");
        let json = serde_json::to_string(&context).expect("serialize");
        assert!(!json.contains("visitorData"), "{json}");
        assert!(json.contains(r#""clientVersion":"#), "{json}");
        // Falls back to the profile version rather than sending an empty string.
        assert!(json.contains(&profile().version), "{json}");
    }

    #[test]
    fn search_request_serializes_exactly_context_and_query() {
        let request = SearchRequest::new(context(), "captain jack");
        let json = serde_json::to_string(&request).expect("serialize");
        let expected_prefix = r#"{"context":{"client":{"hl":"en""#;
        assert!(json.starts_with(expected_prefix), "{json}");
        assert!(json.ends_with(r#","query":"captain jack"}"#), "{json}");
        assert!(!json.contains("params"), "{json}");
    }

    #[test]
    fn search_request_places_params_after_query() {
        let request = SearchRequest::with_params(context(), "q", "EgIQAQ%3D%3D");
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(
            json.ends_with(r#","query":"q","params":"EgIQAQ%3D%3D"}"#),
            "{json}"
        );
        assert!(request.has_params());
        assert_eq!(request.params(), Some("EgIQAQ%3D%3D"));
    }

    #[test]
    fn continuation_request_cannot_carry_a_query() {
        let request = ContinuationRequest::new(context(), "CBQSCBg");
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.ends_with(r#","continuation":"CBQSCBg"}"#), "{json}");
        assert!(!json.contains("query"), "{json}");
        assert!(!json.contains("params"), "{json}");
    }

    #[test]
    fn player_request_serializes_the_verified_shape() {
        let request = PlayerRequest::new(context(), "9Umm2MAKsMc");
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(
            json.ends_with(r#","videoId":"9Umm2MAKsMc","contentCheckOk":true,"racyCheckOk":true}"#),
            "{json}"
        );
    }

    #[test]
    fn documented_bodies_deserialize_back_into_the_types() {
        // Guards against a future field rename silently breaking the contract in one direction only.
        let search: SearchRequest = serde_json::from_str(
            r#"{"context":{"client":{"hl":"en","gl":"US","clientName":"WEB","clientVersion":"2.TEST","originalUrl":"https://www.youtube.com"},"user":{"lockedSafetyMode":false},"request":{"useSsl":true,"internalExperimentFlags":[]}},"query":"q"}"#,
        )
        .expect("search body");
        assert_eq!(search.query, "q");
        assert!(!search.has_params());

        let continuation: ContinuationRequest = serde_json::from_str(
            r#"{"context":{"client":{"hl":"en","gl":"US","clientName":"WEB","clientVersion":"2.TEST","originalUrl":"https://www.youtube.com"},"user":{"lockedSafetyMode":false},"request":{"useSsl":true,"internalExperimentFlags":[]}},"continuation":"TOKEN"}"#,
        )
        .expect("continuation body");
        assert_eq!(continuation.continuation, "TOKEN");
    }
}
