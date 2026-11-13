//! Validated, provider-neutral identifiers.
//!
//! Every identifier that can reach a provider URL is parsed into a newtype here first
//! (`docs/architecture/API_CONTRACTS.md` §4). This is both input validation and SSRF hardening:
//! a caller can never smuggle path segments or query parameters into an upstream request.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use vydra_errors::{Error, Result};

/// Maximum accepted length of any identifier. Guards against pathological input.
const MAX_ID_LEN: usize = 64;

fn check_charset(id: &str, allowed: impl Fn(char) -> bool) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.chars().all(|c| allowed(c))
}

fn is_bare_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

macro_rules! id_newtype {
    (
        $(#[$meta:meta])*
        $name:ident, $field:literal, $validate:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Parse and validate. Returns a `validation_failed` error naming the field.
            pub fn parse(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                #[allow(clippy::redundant_closure_call)]
                let ok = ($validate)(&value);
                if ok {
                    Ok(Self(value))
                } else {
                    Err(Error::validation($field, "invalid_format"))
                }
            }

            /// Parse a known-good identifier (e.g. from a recorded fixture) without validation.
            ///
            /// Only for internal use where the value comes from a fixture or an upstream response
            /// that has already been validated.
            pub fn new_unchecked(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }

            pub fn as_bytes(&self) -> &[u8] {
                self.0.as_bytes()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self> {
                Self::parse(s)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = Error;

            fn try_from(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

id_newtype!(
    /// A YouTube video id: exactly 11 URL-safe base64 characters.
    VideoId,
    "id",
    |v: &str| v.len() == 11 && check_charset(v, is_bare_id_char)
);

id_newtype!(
    /// A channel id: `UC` followed by 22 URL-safe base64 characters.
    ChannelId,
    "id",
    |v: &str| v.len() == 24 && v.starts_with("UC") && check_charset(v, is_bare_id_char)
);

id_newtype!(
    /// A playlist id: known prefixes plus URL-safe base64 characters.
    PlaylistId,
    "id",
    |v: &str| {
        const PREFIXES: [&str; 6] = ["PL", "UU", "LL", "OL", "RD", "FL"];
        v.len() >= 12
            && v.len() <= MAX_ID_LEN
            && PREFIXES.iter().any(|p| v.starts_with(p))
            && check_charset(v, is_bare_id_char)
    }
);

id_newtype!(
    /// A channel handle such as `@somebody`.
    ChannelHandle,
    "handle",
    |v: &str| v.len() > 1 && v.len() <= 40 && v.starts_with('@') && check_charset(v, is_bare_id_char)
);

/// Either a channel id (`UC…`) or a handle (`@…`).
///
/// Resolution to a concrete [`ChannelId`] requires an upstream lookup and is the channel engine's
/// responsibility; providers may not invent an id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelRef {
    Id(ChannelId),
    Handle(ChannelHandle),
}

impl ChannelRef {
    pub fn parse(value: &str) -> Result<Self> {
        if value.starts_with('@') {
            ChannelHandle::parse(value).map(Self::Handle)
        } else {
            ChannelId::parse(value).map(Self::Id)
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Id(id) => id.as_str(),
            Self::Handle(handle) => handle.as_str(),
        }
    }
}

impl fmt::Display for ChannelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ChannelRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ChannelRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_video_id() {
        let id = VideoId::parse("dQw4w9WgXcQ").expect("valid id");
        assert_eq!(id.as_str(), "dQw4w9WgXcQ");
        assert_eq!(id.to_string(), "dQw4w9WgXcQ");
    }

    #[test]
    fn rejects_short_and_long_video_ids() {
        assert!(VideoId::parse("short").is_err());
        assert!(VideoId::parse("dQw4w9WgXcQextra").is_err());
    }

    #[test]
    fn rejects_injection_attempts() {
        // Path traversal / query smuggling must be impossible by construction.
        for bad in [
            "../../etc/passwd",
            "dQw4w9Wg?x=1",
            "dQw4w9Wg/Xc",
            "",
            "dQw4w9Wg cQ",
            "dQw4w9WgXcQ\n",
        ] {
            assert!(VideoId::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn channel_ref_dispatches_on_prefix() {
        let handle = ChannelRef::parse("@rustlang").expect("handle");
        assert!(matches!(handle, ChannelRef::Handle(_)));
        let id = ChannelRef::parse("UCBv7HEHvY6Y0a0dS0R5mQ0w").expect("id");
        assert!(matches!(id, ChannelRef::Id(_)));
    }

    #[test]
    fn playlist_prefixes_are_enforced() {
        assert!(PlaylistId::parse("PLabcdefghijkl").is_ok());
        assert!(PlaylistId::parse("RBabcdefghijkl").is_err());
    }

    #[test]
    fn serde_round_trip_is_validated() {
        let id: VideoId = serde_json::from_str("\"dQw4w9WgXcQ\"").expect("deserialize");
        assert_eq!(id.as_str(), "dQw4w9WgXcQ");
        assert!(serde_json::from_str::<VideoId>("\"nope\"").is_err());
        assert_eq!(serde_json::to_string(&id).expect("serialize"), "\"dQw4w9WgXcQ\"");
    }
}
