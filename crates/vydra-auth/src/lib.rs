//! API keys, quotas and rate limiting.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use vydra_config::AuthConfig;
use vydra_errors::{Error, FieldIssue};

/// Identifier of the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Stable key id, or `anonymous`.
    pub key_id: String,
    pub anonymous: bool,
}

impl Principal {
    pub fn anonymous() -> Self {
        Self {
            key_id: "anonymous".into(),
            anonymous: true,
        }
    }
}

/// A stored API key. Only the hash is retained; the secret never leaves the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: String,
    pub prefix: String,
    pub secret_hash: [u8; 32],
    pub monthly_quota: u64,
}

/// Hash a secret with the domain-separated scheme used at rest.
pub fn hash_secret(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"vydra-api-key-v1:");
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Constant-time comparison to avoid a timing oracle on key verification.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Parse a key of the form `vydra_<prefix>_<secret>`.
pub fn parse_key(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let without = raw.strip_prefix("vydra_")?;
    let (prefix, secret) = without.split_once('_')?;
    if prefix.is_empty() || secret.len() < 8 {
        return None;
    }
    Some((prefix.to_owned(), secret.to_owned()))
}

/// Verifies API keys. In v1 the key set comes from configuration/bootstrap rows; the database
/// repository returns the same shape so both paths converge.
#[derive(Debug)]
pub struct Authenticator {
    required: bool,
    keys: Vec<ApiKey>,
}

impl Authenticator {
    pub fn new(required: bool, keys: Vec<ApiKey>) -> Self {
        Self { required, keys }
    }

    /// Build from configuration, registering the development bootstrap key when present.
    pub fn from_config(config: &AuthConfig) -> Result<Self, Error> {
        let mut keys = Vec::new();
        if let Some(bootstrap) = &config.bootstrap_key {
            let raw = bootstrap.expose();
            let Some((prefix, secret)) = parse_key(raw) else {
                return Err(Error::Validation(vec![FieldIssue::new(
                    "auth.bootstrap_key",
                    "invalid_format",
                )]));
            };
            keys.push(ApiKey {
                id: format!("bootstrap-{prefix}"),
                prefix,
                secret_hash: hash_secret(&secret),
                monthly_quota: config.monthly_quota,
            });
        }
        Ok(Self {
            required: config.required,
            keys,
        })
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub fn authenticate(&self, header: Option<&str>) -> Result<Principal, Error> {
        let Some(raw) = header.map(str::trim).filter(|value| !value.is_empty()) else {
            return if self.required {
                Err(Error::Unauthorized("missing API key".into()))
            } else {
                Ok(Principal::anonymous())
            };
        };

        let Some((prefix, secret)) = parse_key(raw) else {
            return Err(Error::Unauthorized("malformed API key".into()));
        };
        let hash = hash_secret(&secret);
        for key in &self.keys {
            if key.prefix == prefix && constant_time_eq(&key.secret_hash, &hash) {
                return Ok(Principal {
                    key_id: key.id.clone(),
                    anonymous: false,
                });
            }
        }
        Err(Error::Unauthorized("unknown API key".into()))
    }
}

struct Bucket {
    window_start: Instant,
    count: u32,
}

/// Fixed-window limiter. Redis-backed counters are Phase 8; this keeps single-node deployments
/// honest in the meantime and is deliberately simple to make its failure mode obvious.
#[derive(Debug)]
pub struct RateLimiter {
    window: Duration,
    buckets: Mutex<HashMap<String, Bucket>>,
    max_buckets: usize,
}

impl RateLimiter {
    pub fn new(window: Duration, max_buckets: usize) -> Self {
        Self {
            window,
            buckets: Mutex::new(HashMap::new()),
            max_buckets: max_buckets.max(16),
        }
    }

    /// Check and consume one unit for `bucket`. Returns `RateLimited` when exhausted.
    pub fn check(&self, bucket: &str, limit: u32) -> Result<(), Error> {
        if limit == 0 {
            return Err(Error::RateLimited { retry_after: None });
        }
        let now = Instant::now();
        let Ok(mut buckets) = self.buckets.lock() else {
            return Ok(());
        };
        if buckets.len() > self.max_buckets {
            buckets.retain(|_, entry| now.duration_since(entry.window_start) < self.window);
        }
        let entry = buckets.entry(bucket.to_owned()).or_insert(Bucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(entry.window_start) >= self.window {
            entry.window_start = now;
            entry.count = 0;
        }
        if entry.count >= limit {
            let remaining = self.window.saturating_sub(now.duration_since(entry.window_start));
            return Err(Error::RateLimited {
                retry_after: Some(remaining.max(Duration::from_millis(1))),
            });
        }
        entry.count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_hashed_and_verified() {
        let config = AuthConfig {
            bootstrap_key: Some(vydra_config::SecretString::new("vydra_dev_abcdefgh")),
            required: true,
            ..Default::default()
        };
        let auth = Authenticator::from_config(&config).expect("auth");
        assert!(auth
            .authenticate(Some("vydra_dev_abcdefgh"))
            .expect("valid")
            .key_id
            .starts_with("bootstrap-"));
        assert!(auth.authenticate(Some("vydra_dev_wrongsecret")).is_err());
        assert!(auth.authenticate(None).is_err());
    }

    #[test]
    fn anonymous_is_allowed_when_auth_is_optional() {
        let auth = Authenticator::new(false, vec![]);
        assert!(auth.authenticate(None).expect("anon").anonymous);
    }

    #[test]
    fn malformed_keys_are_rejected() {
        assert!(parse_key("nope").is_none());
        assert!(parse_key("vydra__short").is_none());
        assert!(parse_key("vydra_pfx_secretsecret").is_some());
    }

    #[test]
    fn rate_limiter_enforces_the_window() {
        let limiter = RateLimiter::new(Duration::from_secs(60), 32);
        for _ in 0..3 {
            limiter.check("k", 3).expect("within limit");
        }
        let error = limiter.check("k", 3).expect_err("must be limited");
        assert!(matches!(error, Error::RateLimited { retry_after: Some(_) }));
        // A different bucket is unaffected.
        limiter.check("other", 3).expect("separate bucket");
    }

    #[test]
    fn constant_time_equality_is_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
