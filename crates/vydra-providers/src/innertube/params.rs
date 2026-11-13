//! InnerTube `params` encoding (protobuf varints, base64url).
//!
//! # Verification status
//!
//! The layout below follows the protobuf field numbers used by the reference implementations
//! (`rustypipe/src/param/search_filter.rs`, `invidious/src/invidious/search/filters.cr`,
//! `MediaServiceCore/.../search/SearchFilterHelper.java`) — see
//! `docs/research/EXTRACTION_METHODS.md` §5.1.
//!
//! **It has not been verified against the live API from this environment.** Therefore:
//! - the provider only declares `SEARCH_FILTERS` when the operator explicitly enables unverified
//!   filters (default: off);
//! - feature filters (bit ids borrowed from the references) are behind the same switch;
//! - the unit tests assert our own encoding is stable, **not** that YouTube accepts it.
//!
//! This is the difference between "implemented" and "verified" that the mission requires us to keep
//! explicit (`docs/PROGRESS.md`).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use vydra_errors::ProviderError;

use crate::{SearchDate, SearchDuration, SearchFeature, SearchFilters, SearchSort};

/// Append a protobuf base-128 varint.
fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(((value & 0x7F) as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Append a `field` (wire type 0, varint) entry.
fn write_field_varint(out: &mut Vec<u8>, field: u32, value: u64) {
    write_varint(out, u64::from(field) << 3);
    write_varint(out, value);
}

fn sort_code(sort: SearchSort) -> Option<u64> {
    match sort {
        SearchSort::Relevance => None,
        SearchSort::UploadDate => Some(1),
        SearchSort::ViewCount => Some(2),
        SearchSort::Rating => Some(3),
    }
}

fn date_code(date: SearchDate) -> u64 {
    match date {
        SearchDate::Hour => 1,
        SearchDate::Day => 2,
        SearchDate::Week => 3,
        SearchDate::Month => 4,
        SearchDate::Year => 5,
    }
}

fn duration_code(duration: SearchDuration) -> u64 {
    match duration {
        SearchDuration::Short => 1,
        SearchDuration::Medium => 2,
        SearchDuration::Long => 3,
    }
}

fn feature_code(feature: SearchFeature) -> u64 {
    // Feature ids as used by the references; see module docs for verification status.
    match feature {
        SearchFeature::Hd => 4,
        SearchFeature::Subtitles => 5,
        SearchFeature::CreativeCommons => 6,
        SearchFeature::ThreeD => 7,
        SearchFeature::Live => 8,
        SearchFeature::Uhd4k => 14,
        SearchFeature::ThreeSixty => 15,
        SearchFeature::Hdr => 25,
        SearchFeature::Vr180 => 26,
    }
}

/// Encode search filters into an InnerTube `params` token.
///
/// Returns `None` when the filters are empty (Relevance + no date/duration/features), because an
/// absent `params` field is what the references send for the default search.
pub fn encode_search_filters(filters: &SearchFilters) -> Result<Option<String>, ProviderError> {
    let mut out: Vec<u8> = Vec::new();

    if let Some(code) = sort_code(filters.sort) {
        write_field_varint(&mut out, 1, code);
    }
    if let Some(date) = filters.uploaded {
        write_field_varint(&mut out, 2, date_code(date));
    }
    if let Some(duration) = filters.duration {
        write_field_varint(&mut out, 3, duration_code(duration));
    }
    for feature in &filters.features {
        write_field_varint(&mut out, 4, feature_code(*feature));
    }

    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(URL_SAFE_NO_PAD.encode(out)))
    }
}

/// Parse a `params` token back into filters. Used by tests and diagnostics only.
pub fn decode_search_filters(token: &str) -> Result<SearchFilters, ProviderError> {
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| ProviderError::InvalidRequest {
        field: "cursor".into(),
        issue: "invalid_params".into(),
    })?;

    let mut filters = SearchFilters::default();
    let mut index = 0usize;
    while index < bytes.len() {
        let (tag, read) = read_varint(&bytes[index..])?;
        index += read;
        let field = (tag >> 3) as u32;
        let wire_type = tag & 0x07;
        if wire_type != 0 {
            return Err(ProviderError::InvalidRequest {
                field: "params".into(),
                issue: "unsupported_wire_type".into(),
            });
        }
        let (value, read) = read_varint(&bytes[index..])?;
        index += read;
        match field {
            1 => {
                filters.sort = match value {
                    1 => SearchSort::UploadDate,
                    2 => SearchSort::ViewCount,
                    3 => SearchSort::Rating,
                    _ => SearchSort::Relevance,
                }
            }
            2 => {
                filters.uploaded = Some(match value {
                    1 => SearchDate::Hour,
                    2 => SearchDate::Day,
                    3 => SearchDate::Week,
                    4 => SearchDate::Month,
                    _ => SearchDate::Year,
                })
            }
            3 => {
                filters.duration = Some(match value {
                    1 => SearchDuration::Short,
                    2 => SearchDuration::Medium,
                    _ => SearchDuration::Long,
                })
            }
            4 => {
                if let Some(feature) = feature_from_code(value) {
                    filters.features.push(feature);
                }
            }
            _ => {}
        }
    }
    Ok(filters)
}

fn read_varint(bytes: &[u8]) -> Result<(u64, usize), ProviderError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok((result, index + 1));
        }
        shift += 7;
    }
    Err(ProviderError::InvalidRequest {
        field: "params".into(),
        issue: "truncated_varint".into(),
    })
}

fn feature_from_code(code: u64) -> Option<SearchFeature> {
    match code {
        4 => Some(SearchFeature::Hd),
        5 => Some(SearchFeature::Subtitles),
        6 => Some(SearchFeature::CreativeCommons),
        7 => Some(SearchFeature::ThreeD),
        8 => Some(SearchFeature::Live),
        14 => Some(SearchFeature::Uhd4k),
        15 => Some(SearchFeature::ThreeSixty),
        25 => Some(SearchFeature::Hdr),
        26 => Some(SearchFeature::Vr180),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filters_produce_no_params() {
        assert_eq!(encode_search_filters(&SearchFilters::default()), Ok(None));
    }

    #[test]
    fn sort_only_encodes_field_one() {
        let filters = SearchFilters {
            sort: SearchSort::UploadDate,
            ..Default::default()
        };
        let token = encode_search_filters(&filters).expect("encode").expect("some");
        // field 1, varint 1 → 0x08 0x01 → base64url "CAE"
        assert_eq!(token, "CAE");
    }

    #[test]
    fn date_and_duration_round_trip() {
        let filters = SearchFilters {
            sort: SearchSort::ViewCount,
            uploaded: Some(SearchDate::Week),
            duration: Some(SearchDuration::Long),
            features: vec![SearchFeature::Hd, SearchFeature::Live],
        };
        let token = encode_search_filters(&filters)
            .expect("encode")
            .expect("some");
        let decoded = decode_search_filters(&token).expect("decode");
        assert_eq!(decoded, filters);
    }

    #[test]
    fn varint_uses_multiple_bytes_for_large_values() {
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        assert_eq!(out, vec![0xAC, 0x02]);
    }
}
