//! Caption documents.

use serde::{Deserialize, Serialize};

use crate::common::{CaptionTrack, Provenance};

/// Document returned by `GET /videos/{id}/captions` (metadata only in v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionTrackList {
    pub items: Vec<CaptionTrack>,
    pub provenance: Provenance,
}

impl CaptionTrackList {
    /// Language codes available, in provider order.
    pub fn language_codes(&self) -> Vec<&str> {
        self.items
            .iter()
            .map(|track| track.language_code.as_str())
            .collect()
    }

    /// Whether an automatic (ASR) track exists for a language.
    pub fn has_automatic(&self, language_code: &str) -> bool {
        self.items.iter().any(|track| {
            track.language_code == language_code && track.kind == crate::CaptionKind::Automatic
        })
    }
}
