//! Parsing toolkit: tolerant walkers, text/number extraction and the [`ParseReport`].
//!
//! Two lessons from research are encoded here:
//!
//! 1. **Tolerate, then report** (rustypipe's `MapResult`, YouTube.js' parse warnings). A page with one
//!    broken item must not become an outage; it must become a warning that tests and dashboards can
//!    see (`docs/research/ARCHITECTURE_COMPARISON.md` §2).
//! 2. **Do not hardcode a single wrapper path.** YouTube renames wrapper levels
//!    (`twoColumnSearchResultsRenderer` → …). We discover renderers by key anywhere in the tree and
//!    count what we do not recognize, instead of navigating a brittle path
//!    (`docs/research/EXTRACTION_METHODS.md` §5).

use serde_json::Value;
use vydra_domain::Thumbnail;
use vydra_errors::ProviderError;

use crate::Strictness;
use vydra_common::text::normalize_thumbnail_url;

/// Renderer keys Vydra understands today.
pub const KNOWN_RENDERERS: &[&str] = &[
    "videoRenderer",
    "compactVideoRenderer",
    "gridVideoRenderer",
    "playlistVideoRenderer",
    "playlistPanelVideoRenderer",
    "channelRenderer",
    "gridChannelRenderer",
    "playlistRenderer",
    "gridPlaylistRenderer",
    "lockupViewModel",
    "channelVideoPlayerRenderer",
];

/// Maximum object depth walked. Guards against pathological/fuzzed payloads.
const MAX_DEPTH: usize = 48;
/// Maximum objects visited per parse. Guards against CPU exhaustion.
const MAX_NODES: usize = 100_000;
/// Maximum warnings retained per report.
const MAX_WARNINGS: usize = 24;

/// Non-fatal parsing diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseReport {
    warnings: Vec<String>,
    unknown_renderers: u32,
    skipped_items: u32,
    missing_fields: u32,
    structural: bool,
}

impl ParseReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(message.into());
        }
    }

    pub fn unknown_renderer(&mut self, key: &str) {
        self.unknown_renderers = self.unknown_renderers.saturating_add(1);
        self.warn(format!("unknown_renderer:{key}"));
    }

    pub fn skipped_item(&mut self, reason: &str) {
        self.skipped_items = self.skipped_items.saturating_add(1);
        self.warn(format!("skipped_item:{reason}"));
    }

    pub fn missing_field(&mut self, renderer: &str, field: &str) {
        self.missing_fields = self.missing_fields.saturating_add(1);
        self.warn(format!("missing_field:{renderer}.{field}"));
    }

    /// Mark that the payload was not the shape we expected at all.
    pub fn structural_problem(&mut self, message: &str) {
        self.structural = true;
        self.warn(format!("structural:{message}"));
    }

    pub fn count(&self) -> u32 {
        self.unknown_renderers
            .saturating_add(self.skipped_items)
            .saturating_add(self.missing_fields)
            .saturating_add(u32::from(self.structural))
    }

    pub fn unknown_renderers(&self) -> u32 {
        self.unknown_renderers
    }

    pub fn skipped_items(&self) -> u32 {
        self.skipped_items
    }

    pub fn has_structural_problem(&self) -> bool {
        self.structural
    }

    pub fn messages(&self) -> &[String] {
        &self.warnings
    }

    pub fn merge(&mut self, other: ParseReport) {
        self.unknown_renderers += other.unknown_renderers;
        self.skipped_items += other.skipped_items;
        self.missing_fields += other.missing_fields;
        self.structural |= other.structural;
        for message in other.warnings {
            if self.warnings.len() < MAX_WARNINGS {
                self.warnings.push(message);
            }
        }
    }

    /// In `Strict` mode any warning fails the parse (used by fixture tests so regressions are loud).
    pub fn check(&self, strictness: Strictness) -> Result<(), ProviderError> {
        if strictness == Strictness::Strict && self.count() > 0 {
            return Err(ProviderError::Parse {
                message: format!(
                    "strict parse failed: {} warning(s): {}",
                    self.count(),
                    self.warnings.join("; ")
                ),
            });
        }
        Ok(())
    }
}

/// A discovered renderer object.
#[derive(Debug, Clone)]
pub struct Renderer<'a> {
    pub key: &'a str,
    pub value: &'a Value,
}

/// Recursively collect known renderer objects and count unknown `*Renderer` keys.
///
/// The walk is breadth-first over objects/arrays with explicit depth and node budgets so that a
/// hostile response cannot exhaust the process.
pub fn collect_renderers<'a>(root: &'a Value, report: &mut ParseReport) -> Vec<Renderer<'a>> {
    let mut found = Vec::new();
    let mut stack: Vec<(&'a Value, usize)> = vec![(root, 0)];
    let mut visited = 0usize;

    while let Some((node, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            report.warn("node_budget_exhausted");
            break;
        }
        if depth > MAX_DEPTH {
            report.warn("depth_budget_exhausted");
            continue;
        }

        match node {
            Value::Object(map) => {
                for (key, value) in map {
                    if key.ends_with("Renderer") || key == "lockupViewModel" || key == "videoRenderer" {
                        if KNOWN_RENDERERS.contains(&key.as_str()) {
                            found.push(Renderer { key, value });
                            // Do not descend into a consumed renderer: its inner renderers belong
                            // to a different resource (e.g. channelRenderer inside videoRenderer).
                            continue;
                        }
                        if key.ends_with("Renderer") && !value.is_null() {
                            report.unknown_renderer(key);
                        }
                    }
                    if value.is_object() || value.is_array() {
                        stack.push((value, depth + 1));
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    if item.is_object() || item.is_array() {
                        stack.push((item, depth + 1));
                    }
                }
            }
            _ => {}
        }
    }

    found
}

/// Extract a text value from any of YouTube's text shapes:
/// `"plain string"`, `{"simpleText": …}`, `{"runs": [{"text": …}]}`, `{"content": …}`.
pub fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        Value::Object(map) => {
            if let Some(simple) = map.get("simpleText").and_then(Value::as_str) {
                let trimmed = simple.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            if let Some(runs) = map.get("runs").and_then(Value::as_array) {
                let joined: String = runs
                    .iter()
                    .filter_map(|run| run.get("text").and_then(Value::as_str))
                    .collect();
                let trimmed = joined.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            if let Some(content) = map.get("content").and_then(Value::as_str) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            None
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Parse an unsigned integer that YouTube may encode as a number or a numeric string.
pub fn number_of(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Extract thumbnails from `{"thumbnails":[…]}` or a bare array, normalizing URLs.
pub fn thumbnails_of(value: &Value) -> Vec<Thumbnail> {
    let array = match value {
        Value::Array(items) => items,
        Value::Object(map) => match map.get("thumbnails").and_then(Value::as_array) {
            Some(items) => items,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };

    array
        .iter()
        .filter_map(|item| {
            let raw = item.get("url").and_then(Value::as_str)?;
            let url = normalize_thumbnail_url(raw)?;
            Some(Thumbnail {
                url,
                width: item.get("width").and_then(Value::as_u64).map(|v| v as u32),
                height: item.get("height").and_then(Value::as_u64).map(|v| v as u32),
            })
        })
        .collect()
}

/// Find the first `continuationCommand.token` anywhere in the tree.
///
/// Continuation tokens are opaque; every reference implementation round-trips them unresolved
/// (`docs/research/EXTRACTION_METHODS.md` §5.3).
pub fn find_continuation(root: &Value) -> Option<String> {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return None;
        }
        match node {
            Value::Object(map) => {
                if let Some(command) = map.get("continuationCommand") {
                    if let Some(token) = command.get("token").and_then(Value::as_str) {
                        if !token.is_empty() {
                            return Some(token.to_owned());
                        }
                    }
                }
                for value in map.values() {
                    if value.is_object() || value.is_array() {
                        stack.push(value);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    if item.is_object() || item.is_array() {
                        stack.push(item);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract a JSON object that follows `marker` in an HTML document (`var ytInitialData = {…}`).
///
/// Uses brace matching with string-literal awareness, capped by `max_len`. This is the recovery
/// technique documented in `FatihArridho/Unofficial-YoutubeApi/lib/youtube.js#extractYtInitialData`
/// and is only used by HTML fallbacks — never as a primary path.
pub fn extract_json_after(source: &str, marker: &str, max_len: usize) -> Option<Value> {
    let start = source.find(marker)? + marker.len();
    let bytes = source.as_bytes();
    let mut index = start;

    // Skip whitespace and an optional '=' / ':' separator.
    while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'=' || bytes[index] == b':') {
        index += 1;
    }
    if index >= bytes.len() || bytes[index] != b'{' {
        return None;
    }

    let begin = index;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let limit = begin.saturating_add(max_len);

    while index < bytes.len() && index < limit {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        let slice = &source[begin..=index];
                        return serde_json::from_str(slice).ok();
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

/// Find `"KEY":"VALUE"` in a JS/JSON blob without a full parse (bootstrap values from HTML).
pub fn find_json_string_field(source: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let mut search_from = 0usize;
    while let Some(offset) = source[search_from..].find(&needle) {
        let value_start = search_from + offset + needle.len();
        let mut escaped = false;
        let mut value_end = value_start;
        for (i, ch) in source[value_start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => {
                    value_end = value_start + i;
                    break;
                }
                _ => {}
            }
        }
        if value_end > value_start {
            return Some(source[value_start..value_end].to_owned());
        }
        search_from = value_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_renderers_anywhere_in_the_tree() {
        let payload = json!({
            "contents": {
                "twoColumnSearchResultsRenderer": {
                    "primaryContents": {
                        "sectionListRenderer": {
                            "contents": [{
                                "itemSectionRenderer": {
                                    "contents": [
                                        {"videoRenderer": {"videoId": "dQw4w9WgXcQ"}},
                                        {"channelRenderer": {"channelId": "UCBv7HEHvY6Y0a0dS0R5mQ0wX"}},
                                        {"unknownThingRenderer": {"x": 1}}
                                    ]
                                }
                            }]
                        }
                    }
                }
            }
        });

        let mut report = ParseReport::new();
        let renderers = collect_renderers(&payload, &mut report);
        let keys: Vec<&str> = renderers.iter().map(|r| r.key).collect();
        assert!(keys.contains(&"videoRenderer"));
        assert!(keys.contains(&"channelRenderer"));
        assert_eq!(report.unknown_renderers(), 1);
        assert!(report.messages().iter().any(|m| m.contains("unknownThingRenderer")));
    }

    #[test]
    fn depth_budget_stops_pathological_nesting() {
        let mut value = json!({"leaf": true});
        for _ in 0..200 {
            value = json!({ "nestedRendererDepth": { "child": value }, "plain": { "x": value } });
        }
        let mut report = ParseReport::new();
        let _ = collect_renderers(&value, &mut report);
        assert!(report.messages().iter().any(|m| m.contains("depth_budget")));
    }

    #[test]
    fn text_shapes_are_handled() {
        assert_eq!(text_of(&json!("hello")), Some("hello".into()));
        assert_eq!(text_of(&json!({"simpleText": "hi"})), Some("hi".into()));
        assert_eq!(
            text_of(&json!({"runs": [{"text": "a"}, {"text": "b"}]})),
            Some("ab".into())
        );
        assert_eq!(text_of(&json!({"runs": []})), None);
        assert_eq!(text_of(&json!(null)), None);
    }

    #[test]
    fn continuation_is_discovered() {
        let payload = json!({
            "contents": [{"continuationItemRenderer": {"continuationEndpoint": {
                "continuationCommand": {"token": "CBQSCBg"}
            }}}]
        });
        assert_eq!(find_continuation(&payload), Some("CBQSCBg".into()));
    }

    #[test]
    fn html_fallback_extracts_json_with_braces_inside_strings() {
        let html = r#"<script>var ytInitialData = {"a":"} not a brace {","b":[1,2]};</script>"#;
        let value = extract_json_after(html, "var ytInitialData", 4096).expect("json");
        assert_eq!(value["a"], "} not a brace {");
        assert_eq!(value["b"][1], 2);
    }

    #[test]
    fn html_fallback_returns_none_when_truncated() {
        let html = r#"<script>var ytInitialData = {"a":"unterminated""#;
        assert!(extract_json_after(html, "var ytInitialData", 4096).is_none());
    }

    #[test]
    fn json_string_field_is_found_after_other_keys() {
        let js = r#"ytcfg.set({"X":"1","INNERTUBE_API_KEY":"AIzaSyTEST","VISITOR_DATA":"abc"});"#;
        assert_eq!(
            find_json_string_field(js, "INNERTUBE_API_KEY"),
            Some("AIzaSyTEST".into())
        );
        assert_eq!(find_json_string_field(js, "MISSING"), None);
    }

    #[test]
    fn strict_mode_turns_warnings_into_errors() {
        let mut report = ParseReport::new();
        report.missing_field("videoRenderer", "title");
        assert!(report.check(Strictness::Lenient).is_ok());
        assert!(matches!(
            report.check(Strictness::Strict),
            Err(ProviderError::Parse { .. })
        ));
    }
}
