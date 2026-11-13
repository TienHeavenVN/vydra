//! Pagination primitives shared by the API edge and the engines.

use vydra_errors::{Error, Result};

/// Default page size when the caller does not specify one.
pub const DEFAULT_LIMIT: u16 = 20;
/// Hard upper bound for a single page (`docs/architecture/API_CONTRACTS.md` §2).
pub const MAX_LIMIT: u16 = 50;

/// Raw pagination query parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageParams {
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

impl PageParams {
    /// Validate and resolve the effective page size.
    ///
    /// `limit=0` and `limit > MAX_LIMIT` are caller errors (not silently clamped) so that clients
    /// cannot believe they received a full page when they did not.
    pub fn effective_limit(&self) -> Result<u16> {
        match self.limit {
            None => Ok(DEFAULT_LIMIT),
            Some(0) => Err(Error::validation("limit", "out_of_range")),
            Some(limit) if limit > MAX_LIMIT => Err(Error::validation("limit", "out_of_range")),
            Some(limit) => Ok(limit),
        }
    }

    /// Cursor as a borrowed string, normalising empty values to `None`.
    pub fn cursor_ref(&self) -> Option<&str> {
        self.cursor
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

/// Pagination metadata attached to every paged response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaginationMeta {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u16,
    pub returned: u16,
}

impl PaginationMeta {
    pub fn new(limit: u16, returned: usize, next_cursor: Option<String>) -> Self {
        Self {
            next_cursor,
            prev_cursor: None,
            limit,
            returned: u16::try_from(returned).unwrap_or(u16::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limit_applies() {
        assert_eq!(PageParams::default().effective_limit(), Ok(DEFAULT_LIMIT));
    }

    #[test]
    fn out_of_range_limits_are_rejected() {
        for bad in [0u16, 51, 1000] {
            let params = PageParams {
                limit: Some(bad),
                cursor: None,
            };
            assert!(params.effective_limit().is_err(), "accepted limit {bad}");
        }
    }

    #[test]
    fn boundary_limits_are_accepted() {
        for good in [1u16, DEFAULT_LIMIT, MAX_LIMIT] {
            let params = PageParams {
                limit: Some(good),
                cursor: None,
            };
            assert_eq!(params.effective_limit(), Ok(good));
        }
    }

    #[test]
    fn empty_cursor_is_none() {
        let params = PageParams {
            limit: None,
            cursor: Some("   ".into()),
        };
        assert_eq!(params.cursor_ref(), None);
    }
}
