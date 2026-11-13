//! Shared primitives for Vydra: validated identifiers, localized text parsing, signed cursors and
//! pagination.
//!
//! This crate deliberately has **no** HTTP, provider or database dependencies: it is usable from
//! every other crate without creating cycles.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod cursor;
pub mod ids;
pub mod pagination;
pub mod text;

pub use cursor::{CursorCodec, CursorPayload, MIN_SECRET_LEN};
pub use ids::{ChannelHandle, ChannelId, ChannelRef, PlaylistId, VideoId};
pub use pagination::{DEFAULT_LIMIT, MAX_LIMIT, PageParams, PaginationMeta};

/// Build metadata exposed by `GET /status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub git_sha: &'static str,
}

impl BuildInfo {
    /// Build information compiled into this binary.
    pub const fn current() -> Self {
        // `CARGO_PKG_VERSION` is always available; the git sha is injected by CI, falling back to
        // "unknown" for local builds so the binary never lies about its provenance.
        Self {
            version: env!("CARGO_PKG_VERSION"),
            git_sha: match option_env!("VYDRA_GIT_SHA") {
                Some(sha) => sha,
                None => "unknown",
            },
        }
    }
}
