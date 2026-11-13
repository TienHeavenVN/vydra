//! Vydra error taxonomy.
//!
//! Three layers exist by design (see `docs/research/IMPLEMENTATION_DECISIONS.md`, ADR-009):
//!
//! 1. [`ProviderError`] — produced by a provider; decides whether fallback/retry is allowed.
//! 2. [`Error`] — produced by engines/adapters (cache, database, auth); the internal type.
//! 3. [`ErrorBody`] — the public JSON contract. Only `vydra-api` serializes it.
//!
//! Core invariant: a *content* fact (`Unavailable`, `NotFound`) never becomes a retryable transport
//! error and never becomes a generic 500.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why content could not be served, as reported by the upstream provider.
///
/// Mirrors the `playabilityStatus.status` / `reason` values observed across the reference
/// implementations (see `docs/research/EXTRACTION_METHODS.md` §6), but provider-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailabilityReason {
    /// Video/channel/playlist no longer exists upstream.
    Deleted,
    /// Uploader restricted visibility.
    Private,
    /// Requires a paid channel membership.
    MembersOnly,
    /// Requires sign-in (age gate or bot suspicion).
    LoginRequired,
    /// Age restricted and no verified account.
    AgeRestricted,
    /// Blocked in the requested content country.
    GeoRestricted,
    /// Live stream is not currently broadcasting.
    NotBroadcasting,
    /// Copyright or policy block.
    CopyrightBlocked,
    /// DRM protected (movies, premium).
    DrmProtected,
    /// Upstream reported some other unavailability.
    Other,
}

impl UnavailabilityReason {
    /// Stable machine-readable identifier used in the public error `details` array.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Private => "private",
            Self::MembersOnly => "members_only",
            Self::LoginRequired => "login_required",
            Self::AgeRestricted => "age_restricted",
            Self::GeoRestricted => "geo_restricted",
            Self::NotBroadcasting => "not_broadcasting",
            Self::CopyrightBlocked => "copyright_blocked",
            Self::DrmProtected => "drm_protected",
            Self::Other => "other",
        }
    }
}

/// A single field-level validation problem, safe to expose to API clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldIssue {
    /// Request field name, e.g. `q`, `limit`, `cursor`, `path.id`.
    pub field: String,
    /// Stable issue code, e.g. `too_long`, `invalid_format`, `out_of_range`.
    pub issue: String,
}

impl FieldIssue {
    pub fn new(field: impl Into<String>, issue: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            issue: issue.into(),
        }
    }
}

/// Errors raised by a provider implementation.
///
/// The split between "content fact" and "transient condition" is the basis of bounded fallback
/// (`docs/architecture/PROVIDER_INTERFACE.md` §3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// Content exists but cannot be served. Deterministic; **no fallback**, never retried.
    #[error("content unavailable ({reason:?}): {message}")]
    Unavailable {
        reason: UnavailabilityReason,
        message: String,
    },

    /// Content does not exist. Deterministic; no fallback.
    #[error("not found: {id} ({message})")]
    NotFound { id: String, message: String },

    /// Request is valid for the API but unsupported by this provider.
    #[error("unsupported capability: {capability}")]
    Unsupported { capability: String },

    /// Caller error (should have been rejected at the edge). No fallback.
    #[error("invalid provider request: {field} ({issue})")]
    InvalidRequest { field: String, issue: String },

    /// Upstream rate limit. Fallback + circuit breaker.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },

    /// Upstream rejected our session/credentials. Invalidate session, then fallback.
    #[error("unauthorized: {message}")]
    Unauthorized { message: String },

    /// Non-2xx upstream response. Fallback + breaker.
    #[error("upstream status {status}: {message}")]
    Upstream { status: u16, message: String },

    /// Deadline exceeded. Fallback + breaker.
    #[error("provider timeout")]
    Timeout,

    /// Connection/TLS/DNS failure. Fallback + breaker.
    #[error("transport error: {message}")]
    Transport { message: String },

    /// Response could not be parsed into the domain minimum. Fallback + alert.
    #[error("parse error: {message}")]
    Parse { message: String },

    /// Circuit breaker open: request was never attempted.
    #[error("provider circuit open")]
    CircuitOpen { retry_after: Option<Duration> },
}

impl ProviderError {
    /// Whether the registry may try another provider after this error.
    ///
    /// This is the single most consequential rule of the provider contract: falling back on
    /// `Unavailable`/`NotFound` wastes quota and hides content facts.
    pub fn is_fallback_eligible(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Unauthorized { .. }
                | Self::Upstream { .. }
                | Self::Timeout
                | Self::Transport { .. }
                | Self::Parse { .. }
        )
    }

    /// Whether the failure should count against provider health.
    pub fn affects_health(&self) -> bool {
        !matches!(
            self,
            Self::Unavailable { .. }
                | Self::NotFound { .. }
                | Self::Unsupported { .. }
                | Self::InvalidRequest { .. }
                | Self::CircuitOpen { .. }
        )
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } | Self::CircuitOpen { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// Stable code used in logs/metrics. Not part of the public API contract.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "unavailable",
            Self::NotFound { .. } => "not_found",
            Self::Unsupported { .. } => "unsupported",
            Self::InvalidRequest { .. } => "invalid_provider_request",
            Self::RateLimited { .. } => "provider_rate_limited",
            Self::Unauthorized { .. } => "provider_unauthorized",
            Self::Upstream { .. } => "provider_upstream",
            Self::Timeout => "provider_timeout",
            Self::Transport { .. } => "provider_transport",
            Self::Parse { .. } => "provider_parse",
            Self::CircuitOpen { .. } => "provider_circuit_open",
        }
    }
}

/// Internal error type used by engines, adapters and handlers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("validation failed")]
    Validation(Vec<FieldIssue>),

    #[error("invalid cursor")]
    InvalidCursor,

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("content unavailable ({reason:?}): {message}")]
    ContentUnavailable {
        reason: UnavailabilityReason,
        message: String,
    },

    #[error("quota exceeded")]
    QuotaExceeded { retry_after: Option<Duration> },

    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },

    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("all providers failed: {0}")]
    ProviderUnavailable(String),

    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    #[error("provider circuit open")]
    ProviderCircuitOpen { retry_after: Option<Duration> },

    #[error("deadline exceeded")]
    Timeout,

    #[error("cache error: {0}")]
    Cache(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn validation(field: impl Into<String>, issue: impl Into<String>) -> Self {
        Self::Validation(vec![FieldIssue::new(field, issue)])
    }

    /// Map a provider failure into the internal error type, preserving content facts.
    pub fn from_provider(err: ProviderError) -> Self {
        match err {
            ProviderError::Unavailable { reason, message } => {
                Self::ContentUnavailable { reason, message }
            }
            ProviderError::NotFound { message, .. } => Self::NotFound(message),
            ProviderError::InvalidRequest { field, issue } => {
                Self::Validation(vec![FieldIssue::new(field, issue)])
            }
            ProviderError::Unsupported { capability } => Self::CapabilityUnavailable(capability),
            ProviderError::CircuitOpen { retry_after } => Self::ProviderCircuitOpen { retry_after },
            ProviderError::RateLimited { retry_after } => Self::RateLimited { retry_after },
            other => Self::Provider(other),
        }
    }
}

/// Machine-readable public error code (closed set, ADR-009).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ValidationFailed,
    InvalidCursor,
    Unauthorized,
    Forbidden,
    NotFound,
    ContentUnavailable,
    QuotaExceeded,
    RateLimited,
    ProviderRateLimited,
    CapabilityUnavailable,
    ProviderUnavailable,
    ProviderCircuitOpen,
    Timeout,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ValidationFailed => "validation_failed",
            Self::InvalidCursor => "invalid_cursor",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::ContentUnavailable => "content_unavailable",
            Self::QuotaExceeded => "quota_exceeded",
            Self::RateLimited => "rate_limited",
            Self::ProviderRateLimited => "provider_rate_limited",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderCircuitOpen => "provider_circuit_open",
            Self::Timeout => "timeout",
            Self::InternalError => "internal_error",
        }
    }

    /// HTTP status for this code. Kept here (not in `vydra-api`) so the contract is testable in a
    /// crate that has no HTTP dependency.
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::ValidationFailed | Self::InvalidCursor => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::ContentUnavailable => 409,
            Self::QuotaExceeded | Self::RateLimited | Self::ProviderRateLimited => 429,
            Self::CapabilityUnavailable | Self::ProviderUnavailable | Self::ProviderCircuitOpen => {
                503
            }
            Self::Timeout => 504,
            Self::InternalError => 500,
        }
    }

    /// Whether a client may retry the identical request later.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::ProviderRateLimited
                | Self::ProviderUnavailable
                | Self::ProviderCircuitOpen
                | Self::Timeout
                | Self::InternalError
        )
    }
}

/// Public error payload. Serialized by `vydra-api` as `{"error": { ... }}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    pub status: u16,
    pub request_id: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<FieldIssue>,
}

impl ErrorBody {
    /// Build the public representation. `request_id` is supplied by the edge middleware.
    ///
    /// Internal messages are deliberately replaced with stable, non-leaking text; the real cause is
    /// only written to logs (ADR-014).
    pub fn from_error(err: &Error, request_id: impl Into<String>) -> Self {
        let request_id = request_id.into();
        match err {
            Error::Validation(issues) => Self {
                code: ErrorCode::ValidationFailed,
                message: "One or more request parameters are invalid.".into(),
                status: ErrorCode::ValidationFailed.status(),
                request_id,
                retryable: false,
                retry_after_ms: None,
                details: issues.clone(),
            },
            Error::InvalidRequest(msg) => {
                Self::plain(ErrorCode::InvalidRequest, msg.clone(), request_id)
            }
            Error::InvalidCursor => Self::plain(
                ErrorCode::InvalidCursor,
                "Cursor is malformed or has expired.",
                request_id,
            ),
            Error::Unauthorized(msg) => {
                Self::plain(ErrorCode::Unauthorized, msg.clone(), request_id)
            }
            Error::Forbidden(msg) => Self::plain(ErrorCode::Forbidden, msg.clone(), request_id),
            Error::NotFound(msg) => Self::plain(ErrorCode::NotFound, msg.clone(), request_id),
            Error::ContentUnavailable { reason, message } => Self {
                code: ErrorCode::ContentUnavailable,
                message: message.clone(),
                status: ErrorCode::ContentUnavailable.status(),
                request_id,
                retryable: false,
                retry_after_ms: None,
                details: vec![FieldIssue::new("availability", reason.as_str())],
            },
            Error::QuotaExceeded { retry_after } => Self {
                code: ErrorCode::QuotaExceeded,
                message: "API key quota exhausted.".into(),
                status: ErrorCode::QuotaExceeded.status(),
                request_id,
                retryable: false,
                retry_after_ms: retry_after.map(ms),
                details: Vec::new(),
            },
            Error::RateLimited { retry_after } => Self {
                retry_after_ms: retry_after.map(ms),
                ..Self::plain_with(
                    ErrorCode::RateLimited,
                    "Request rate limit exceeded.",
                    request_id,
                )
            },
            Error::Provider(err) if matches!(err, ProviderError::RateLimited { .. }) => {
                let retry_after = err.retry_after();
                Self {
                    retry_after_ms: retry_after.map(ms),
                    ..Self::plain_with(
                        ErrorCode::ProviderRateLimited,
                        "Upstream provider rate-limited the request.",
                        request_id,
                    )
                }
            }
            Error::Provider(_) => Self::plain_with(
                ErrorCode::ProviderUnavailable,
                "No provider could serve the request.",
                request_id,
            ),
            Error::ProviderUnavailable(msg) => {
                Self::plain(ErrorCode::ProviderUnavailable, msg.clone(), request_id)
            }
            Error::CapabilityUnavailable(cap) => Self::plain(
                ErrorCode::CapabilityUnavailable,
                format!("No provider implements the required capability ({cap})."),
                request_id,
            ),
            Error::ProviderCircuitOpen { retry_after } => Self {
                retry_after_ms: retry_after.map(ms),
                ..Self::plain_with(
                    ErrorCode::ProviderCircuitOpen,
                    "Providers are temporarily disabled after repeated failures.",
                    request_id,
                )
            },
            Error::Timeout => Self::plain(
                ErrorCode::Timeout,
                "Upstream deadline exceeded.",
                request_id,
            ),
            Error::Cache(_) | Error::Database(_) | Error::Internal(_) => Self::plain_with(
                ErrorCode::InternalError,
                "An internal error occurred.",
                request_id,
            ),
        }
    }

    fn plain(code: ErrorCode, message: impl Into<String>, request_id: String) -> Self {
        let status = code.status();
        Self {
            code,
            message: message.into(),
            status,
            request_id,
            retryable: code.retryable(),
            retry_after_ms: None,
            details: Vec::new(),
        }
    }

    fn plain_with(code: ErrorCode, message: impl Into<String>, request_id: String) -> Self {
        Self::plain(code, message, request_id)
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RID: &str = "req-test";

    #[test]
    fn content_unavailable_is_not_a_404() {
        let body = ErrorBody::from_error(
            &Error::ContentUnavailable {
                reason: UnavailabilityReason::Private,
                message: "This video is private".into(),
            },
            RID,
        );
        assert_eq!(body.code, ErrorCode::ContentUnavailable);
        assert_eq!(body.status, 409);
        assert!(!body.retryable);
        assert_eq!(body.details[0].issue, "private");
    }

    #[test]
    fn unavailable_never_falls_back_but_ratelimit_does() {
        let unavailable = ProviderError::Unavailable {
            reason: UnavailabilityReason::Deleted,
            message: "gone".into(),
        };
        assert!(!unavailable.is_fallback_eligible());
        assert!(!unavailable.affects_health());

        let limited = ProviderError::RateLimited { retry_after: None };
        assert!(limited.is_fallback_eligible());
        assert!(limited.affects_health());

        let missing = ProviderError::NotFound {
            id: "abc".into(),
            message: "nope".into(),
        };
        assert!(!missing.is_fallback_eligible());
    }

    #[test]
    fn internal_details_do_not_leak() {
        let body =
            ErrorBody::from_error(&Error::Internal("postgres: password=hunter2".into()), RID);
        assert_eq!(body.code, ErrorCode::InternalError);
        assert!(!body.message.contains("hunter2"));
        assert_eq!(body.status, 500);
    }

    #[test]
    fn validation_fields_are_preserved() {
        let body = ErrorBody::from_error(
            &Error::Validation(vec![
                FieldIssue::new("q", "too_long"),
                FieldIssue::new("limit", "out_of_range"),
            ]),
            RID,
        );
        assert_eq!(body.details.len(), 2);
        assert_eq!(body.code.as_str(), "validation_failed");
    }
}
