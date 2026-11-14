//! Opaque, signed pagination cursors (ADR-008).
//!
//! Providers paginate with opaque continuation tokens. Exposing them verbatim would (a) leak
//! provider internals, (b) bind clients to a single provider, (c) let a caller replay arbitrary
//! upstream requests. Vydra therefore wraps the provider token in a signed envelope:
//!
//! ```text
//! cursor = base64url(payload_json) "." base64url(hmac_sha256(secret, payload_json))
//! ```
//!
//! The envelope is stateless, so any replica can validate it, and tampering is detected before the
//! provider is ever contacted.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use vydra_errors::{Error, FieldIssue, Result};

type HmacSha256 = Hmac<Sha256>;

/// Bump when the envelope layout changes; old cursors then fail validation cleanly.
const CURSOR_VERSION: u8 = 1;

/// Minimum secret length accepted by the codec.
pub const MIN_SECRET_LEN: usize = 16;

/// Decoded cursor contents. Not part of the public API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPayload {
    /// Envelope version.
    pub v: u8,
    /// Resource kind, e.g. `search.videos`, `playlist.items`.
    pub k: String,
    /// Provider id that minted the token.
    pub p: String,
    /// Provider continuation token (never exposed).
    pub t: String,
    /// Issued-at, unix seconds.
    pub iat: u64,
}

/// Signs and verifies opaque cursors.
#[derive(Clone)]
pub struct CursorCodec {
    secret: Vec<u8>,
    max_age: Duration,
}

impl std::fmt::Debug for CursorCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the secret.
        f.debug_struct("CursorCodec")
            .field("secret", &"<redacted>")
            .field("max_age", &self.max_age)
            .finish()
    }
}

impl CursorCodec {
    /// Create a codec. `max_age` bounds how long a cursor may be replayed.
    pub fn new(secret: impl AsRef<[u8]>, max_age: Duration) -> Result<Self> {
        let secret = secret.as_ref();
        if secret.len() < MIN_SECRET_LEN {
            return Err(Error::Validation(vec![FieldIssue::new(
                "cursor.secret",
                "too_short",
            )]));
        }
        Ok(Self {
            secret: secret.to_vec(),
            max_age,
        })
    }

    fn sign(&self, payload_json: &[u8]) -> Vec<u8> {
        // `new_from_slice` only fails for key types with a fixed invalid length; HMAC accepts any
        // key length, so this cannot fail for a validated secret.
        let mut mac = match HmacSha256::new_from_slice(&self.secret) {
            Ok(mac) => mac,
            Err(_) => return Vec::new(),
        };
        mac.update(payload_json);
        mac.finalize().into_bytes().to_vec()
    }

    /// Mint a cursor for `kind`/`provider`/`token`.
    pub fn encode(&self, kind: &str, provider: &str, token: &str) -> Result<String> {
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Internal("system clock is before the unix epoch".into()))?
            .as_secs();

        let payload = CursorPayload {
            v: CURSOR_VERSION,
            k: kind.to_owned(),
            p: provider.to_owned(),
            t: token.to_owned(),
            iat: issued_at,
        };
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|e| Error::Internal(format!("cursor encode: {e}")))?;
        let signature = self.sign(&payload_json);
        if signature.is_empty() {
            return Err(Error::Internal("cursor signing failed".into()));
        }

        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload_json),
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    /// Verify and decode a cursor. Every failure maps to the same public error so that clients
    /// cannot distinguish "forged" from "expired" (no oracle).
    pub fn decode(&self, raw: &str, expected_kind: &str) -> Result<CursorPayload> {
        let (payload_b64, signature_b64) = raw.split_once('.').ok_or(Error::InvalidCursor)?;

        let payload_json = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| Error::InvalidCursor)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature_b64)
            .map_err(|_| Error::InvalidCursor)?;

        let mut mac = HmacSha256::new_from_slice(&self.secret).map_err(|_| Error::InvalidCursor)?;
        mac.update(&payload_json);
        mac.verify_slice(&signature)
            .map_err(|_| Error::InvalidCursor)?;

        let payload: CursorPayload =
            serde_json::from_slice(&payload_json).map_err(|_| Error::InvalidCursor)?;

        if payload.v != CURSOR_VERSION || payload.k != expected_kind {
            return Err(Error::InvalidCursor);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidCursor)?
            .as_secs();
        if now.saturating_sub(payload.iat) >= self.max_age.as_secs() {
            return Err(Error::InvalidCursor);
        }

        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec() -> CursorCodec {
        CursorCodec::new("0123456789abcdef0123456789abcdef", Duration::from_secs(60))
            .expect("valid codec")
    }

    #[test]
    fn round_trip() {
        let codec = codec();
        let cursor = codec
            .encode("search.videos", "innertube-web", "CBQSC")
            .expect("encode");
        let payload = codec.decode(&cursor, "search.videos").expect("decode");
        assert_eq!(payload.t, "CBQSC");
        assert_eq!(payload.p, "innertube-web");
    }

    #[test]
    fn kind_mismatch_is_rejected() {
        let codec = codec();
        let cursor = codec
            .encode("search.videos", "innertube-web", "t")
            .expect("encode");
        assert_eq!(
            codec.decode(&cursor, "comments.replies"),
            Err(Error::InvalidCursor)
        );
    }

    #[test]
    fn tampering_is_rejected() {
        let codec = codec();
        let cursor = codec
            .encode("search.videos", "innertube-web", "token")
            .expect("encode");
        let (payload, signature) = cursor.split_once('.').expect("split");
        let forged_payload =
            URL_SAFE_NO_PAD.encode(br#"{"v":1,"k":"search.videos","p":"x","t":"y","iat":0}"#);
        let forged = format!("{forged_payload}.{signature}");
        assert_eq!(
            codec.decode(&forged, "search.videos"),
            Err(Error::InvalidCursor)
        );

        let forged_signature = format!("{payload}.{}", URL_SAFE_NO_PAD.encode([0u8; 32]));
        assert_eq!(
            codec.decode(&forged_signature, "search.videos"),
            Err(Error::InvalidCursor)
        );
    }

    #[test]
    fn other_secret_is_rejected() {
        let a = codec();
        let b = CursorCodec::new("another-secret-0123456789", Duration::from_secs(60))
            .expect("valid codec");
        let cursor = a
            .encode("search.videos", "innertube-web", "t")
            .expect("encode");
        assert_eq!(
            b.decode(&cursor, "search.videos"),
            Err(Error::InvalidCursor)
        );
    }

    #[test]
    fn expiry_is_enforced() {
        let codec = CursorCodec::new("0123456789abcdef0123456789abcdef", Duration::from_secs(0))
            .expect("valid codec");
        let cursor = codec.encode("search.videos", "p", "t").expect("encode");
        // `iat` is generated from the wall clock; a zero max-age expires immediately.
        assert_eq!(
            codec.decode(&cursor, "search.videos"),
            Err(Error::InvalidCursor)
        );
    }

    #[test]
    fn short_secret_is_rejected() {
        assert!(CursorCodec::new("short", Duration::from_secs(60)).is_err());
    }

    #[test]
    fn secret_is_not_debug_printed() {
        let rendered = format!("{:?}", codec());
        assert!(!rendered.contains("0123456789abcdef"));
    }
}
