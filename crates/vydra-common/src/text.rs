//! Localized human-text parsing.
//!
//! YouTube renders view counts, durations and dates as *localized strings*. Every reference
//! implementation re-implements these rules; Vydra centralizes them here with tests derived from
//! the documented failure modes (see `docs/research/EXTRACTION_METHODS.md` §12, which cites
//! `FatihArridho/Unofficial-YoutubeApi/lib/youtube.js` and `yt-dlp/yt_dlp/utils/_utils.py`).
//!
//! Policy (ADR-016): if parsing is ambiguous the function returns `None` rather than a guess.

/// Multiplier suffixes observed in localized YouTube strings.
///
/// Ordered longest-first so that `"ribu"` wins over `"ri"`, `"jt"` over `"j"`, etc.
const SUFFIXES: &[(&str, u64)] = &[
    ("billion", 1_000_000_000),
    ("milyar", 1_000_000_000),
    ("miliar", 1_000_000_000),
    ("million", 1_000_000),
    ("thousand", 1_000),
    ("juta", 1_000_000),
    ("ribu", 1_000),
    ("rb", 1_000),
    ("jt", 1_000_000),
    ("mn", 1_000_000),
    ("bn", 1_000_000_000),
    ("rb.", 1_000),
    ("k", 1_000),
    ("m", 1_000_000),
    ("b", 1_000_000_000),
];

/// Split a localized string into (numeric literal, optional multiplier).
fn split_number_and_suffix(input: &str) -> Option<(String, u64)> {
    let mut numeric = String::new();
    let mut rest_start = None;

    for (idx, ch) in input.char_indices() {
        let is_numeric =
            ch.is_ascii_digit() || ch == '.' || ch == ',' || ch == ' ' || ch == '\u{a0}';
        if is_numeric {
            if rest_start.is_none() {
                numeric.push(if ch == '\u{a0}' { ' ' } else { ch });
            }
        } else if rest_start.is_none() {
            rest_start = Some(idx);
            break;
        }
    }

    let numeric = numeric.trim().to_string();
    if numeric.is_empty() || !numeric.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }

    let suffix_token = rest_start.map(|idx| {
        input[idx..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '.')
            .collect::<String>()
            .to_ascii_lowercase()
    });

    let multiplier = match suffix_token {
        None => 1,
        Some(token) => {
            if token.is_empty() {
                // e.g. "1,234 x ditonton" (Indonesian "views") — no suffix, plain number.
                1
            } else {
                // Unknown unit words ("views", "x ditonton") carry no multiplier. We never invent
                // one; the surrounding field already tells us the string is a count.
                SUFFIXES
                    .iter()
                    .find(|(suffix, _)| token == *suffix || token.starts_with(suffix))
                    .map(|(_, mult)| *mult)
                    .unwrap_or(1)
            }
        }
    };

    Some((numeric, multiplier))
}

/// Interpret a numeric portion with `.`/`,` separators.
///
/// Rules (documented, deterministic):
/// - two or more of the same separator with none of the other → thousands separators
/// - both separators present → the *last* one is the decimal separator
/// - a single separator followed by exactly three digits and no multiplier → thousands separator
/// - a single separator otherwise → decimal separator
fn parse_numeric_literal(raw: &str, has_multiplier: bool) -> Option<f64> {
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }

    let dots = cleaned.matches('.').count();
    let commas = cleaned.matches(',').count();

    let normalized = if dots == 0 && commas == 0 {
        cleaned
    } else if dots >= 2 && commas == 0 {
        cleaned.replace('.', "")
    } else if commas >= 2 && dots == 0 {
        cleaned.replace(',', "")
    } else if dots == 1 && commas == 1 {
        let last_dot = cleaned.rfind('.').unwrap_or(0);
        let last_comma = cleaned.rfind(',').unwrap_or(0);
        if last_dot > last_comma {
            cleaned.replace(',', "")
        } else {
            cleaned.replace('.', "").replace(',', ".")
        }
    } else {
        let sep = if dots == 1 { '.' } else { ',' };
        let trailing = cleaned.rsplit(sep).next().map(str::len).unwrap_or(0);
        if trailing == 3 && !has_multiplier {
            cleaned.replace(sep, "")
        } else if sep == ',' {
            cleaned.replace(',', ".")
        } else {
            cleaned
        }
    };

    let parsed: f64 = normalized.parse().ok()?;
    if !parsed.is_finite() || parsed < 0.0 {
        return None;
    }
    Some(parsed)
}

/// Parse a localized view/subscriber count into an integer.
///
/// Handles `"1,234 views"`, `"1.234 x ditonton"`, `"1.2K views"`, `"19,9 jt subscriber"`,
/// `"3.4M"`, `"12 rb tayangan"`, `"No views"` (→ `Some(0)` is **not** produced: unknown → `None`).
pub fn parse_view_count(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (numeric, multiplier) = split_number_and_suffix(trimmed)?;
    let value = parse_numeric_literal(&numeric, multiplier > 1)? * multiplier as f64;
    if value < 0.0 {
        return None;
    }
    // Guard against absurd values (numeric overflow / garbage input).
    if value > 1e15 {
        return None;
    }
    // Values parsed from abbreviated strings are approximate by nature, but are never fabricated:
    // the caller records `precision` based on the presence of a multiplier.
    Some(value.round() as u64)
}

/// Whether a localized count string used an abbreviated multiplier (`K`, `M`, `jt`, …).
pub fn is_abbreviated_count(input: &str) -> bool {
    split_number_and_suffix(input.trim())
        .map(|(_, mult)| mult > 1)
        .unwrap_or(false)
}

/// Parse a clock-style duration (`"1:02:03"`, `"12:34"`, `"45"`, localized `"1.26.43"`).
///
/// Returns `None` for live/premiere markers and for anything non-numeric — a duration must never be
/// guessed.
pub fn parse_clock_duration(input: &str) -> Option<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered == "live" || lowered.starts_with("live") || lowered.contains("premiere") {
        return None;
    }

    let separator = if trimmed.contains(':') {
        ':'
    } else if trimmed.contains('.') {
        '.'
    } else {
        // Bare seconds.
        return trimmed.parse::<u32>().ok();
    };

    let parts: Vec<&str> = trimmed.split(separator).collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut total: u32 = 0;
    for part in &parts {
        let value: u32 = part.trim().parse().ok()?;
        if value > 999 {
            return None;
        }
        total = total.checked_mul(60)?.checked_add(value)?;
    }
    Some(total)
}

/// Normalize a thumbnail URL to absolute `https`.
pub fn normalize_thumbnail_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if trimmed.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return Some(format!("https://{rest}"));
    }
    if trimmed.starts_with("data:") {
        return None;
    }
    Some(format!("https://{trimmed}"))
}

/// Join an array of text runs, dropping empty ones (YouTube's `runs` shape).
pub fn join_runs<'a, I>(runs: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let joined: String = runs.into_iter().collect();
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_plain_and_grouped_counts() {
        assert_eq!(parse_view_count("1234 views"), Some(1234));
        assert_eq!(parse_view_count("1,234 views"), Some(1234));
        assert_eq!(parse_view_count("1.234 x ditonton"), Some(1234));
        assert_eq!(parse_view_count("1.234.567 x ditonton"), Some(1_234_567));
        assert_eq!(parse_view_count("1,234,567 views"), Some(1_234_567));
    }

    #[test]
    fn parses_abbreviated_counts_in_two_locales() {
        assert_eq!(parse_view_count("1.2K views"), Some(1200));
        assert_eq!(parse_view_count("3.4M views"), Some(3_400_000));
        assert_eq!(parse_view_count("2B views"), Some(2_000_000_000));
        assert_eq!(parse_view_count("12 rb tayangan"), Some(12_000));
        assert_eq!(parse_view_count("19,9 jt subscriber"), Some(19_900_000));
        assert_eq!(parse_view_count("896.804 x ditonton"), Some(896_804));
    }

    #[test]
    fn does_not_match_suffix_inside_a_word() {
        // The documented false positive: 'b' inside "subscriber"/"ditonton" neighbours.
        assert_eq!(parse_view_count("5 subscribers"), Some(5));
        assert_eq!(parse_view_count("5 subscriber"), Some(5));
        assert_ne!(parse_view_count("5 subscribers"), Some(5_000_000_000));
    }

    #[test]
    fn rejects_garbage_counts() {
        assert_eq!(parse_view_count(""), None);
        assert_eq!(parse_view_count("No views"), None);
        assert_eq!(parse_view_count("views"), None);
        assert_eq!(parse_view_count("--"), None);
    }

    #[test]
    fn abbreviates_are_reported() {
        assert!(is_abbreviated_count("1.2K views"));
        assert!(!is_abbreviated_count("1234 views"));
    }

    #[test]
    fn parses_clock_durations() {
        assert_eq!(parse_clock_duration("45"), Some(45));
        assert_eq!(parse_clock_duration("12:34"), Some(754));
        assert_eq!(parse_clock_duration("1:02:03"), Some(3723));
        assert_eq!(parse_clock_duration("1.26.43"), Some(5203));
        assert_eq!(parse_clock_duration("LIVE"), None);
        assert_eq!(parse_clock_duration("1:02:03:04"), None);
        assert_eq!(parse_clock_duration("not a duration"), None);
    }

    #[test]
    fn normalizes_thumbnail_urls() {
        assert_eq!(
            normalize_thumbnail_url("//i.ytimg.com/vi/x/hq.jpg"),
            Some("https://i.ytimg.com/vi/x/hq.jpg".into())
        );
        assert_eq!(
            normalize_thumbnail_url("http://i.ytimg.com/vi/x/hq.jpg"),
            Some("https://i.ytimg.com/vi/x/hq.jpg".into())
        );
        assert_eq!(normalize_thumbnail_url("   "), None);
        assert_eq!(normalize_thumbnail_url("data:text/html,x"), None);
    }

    fn arbitrary_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(any::<char>(), 0..40)
            .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        /// Malformed and arbitrary text must never panic (robustness requirement for parsers that
        /// consume upstream-controlled strings).
        #[test]
        fn parsers_never_panic(input in arbitrary_text()) {
            let _ = parse_view_count(&input);
            let _ = parse_clock_duration(&input);
            let _ = normalize_thumbnail_url(&input);
            let _ = is_abbreviated_count(&input);
        }

        /// Round trip: any u64 rendered as plain digits parses back to itself.
        #[test]
        fn plain_numbers_round_trip(value in 0u64..1_000_000_000_000u64) {
            let rendered = format!("{value} views");
            prop_assert_eq!(parse_view_count(&rendered), Some(value));
        }
    }
}
