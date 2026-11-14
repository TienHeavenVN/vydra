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

/// Layout-only renderers: they carry no content of their own, they only group other renderers.
///
/// They must not be reported as unknown: `sectionListRenderer`, `itemSectionRenderer` and friends
/// appear in every single InnerTube response, so counting them turns the `parse_warnings` metric
/// into noise and hides the renderers that actually need mapping.
pub const STRUCTURAL_RENDERERS: &[&str] = &[
    "twoColumnSearchResultsRenderer",
    "twoColumnBrowseResultsRenderer",
    "twoColumnWatchNextResults",
    "sectionListRenderer",
    "itemSectionRenderer",
    "shelfRenderer",
    "horizontalListRenderer",
    "gridRenderer",
    "richGridRenderer",
    "richItemRenderer",
    "expandedShelfContentsRenderer",
    "singleColumnBrowseResultsRenderer",
    "tabRenderer",
    "sectionListContinuationRenderer",
    // A plain vertical list of renderers (present on the `veritasium` capture): layout, nothing else.
    "verticalListRenderer",
    // Carries nothing but the next token: it is navigation, not a result. Counting it made every
    // paginated page look like it had unread content (one occurrence per page on the Phase 4.3
    // captures), for a renderer whose only payload the parser already extracts by key.
    "continuationItemRenderer",
    // Browse-page chrome (Phase 6). Headers, tabs, chips, sort menus and page-level alerts are
    // navigation and metadata — not list items — so they are layout for the purpose of counting
    // unmapped content, and their inner content renderers are still discovered by descending into them.
    "pageHeaderRenderer",
    "c4TabbedHeaderRenderer",
    "channelMetadataRenderer",
    "channelAboutFullMetadataRenderer",
    "channelTaglineRenderer",
    "channelAgeGateRenderer",
    "chipCloudRenderer",
    "chipCloudChipRenderer",
    "feedFilterChipBarRenderer",
    "sortFilterSubMenuRenderer",
    "playlistHeaderRenderer",
    "playlistMetadataRenderer",
    "playlistSidebarPrimaryInfoRenderer",
    "playlistSidebarSecondaryInfoRenderer",
    "videoOwnerRenderer",
    "playlistVideoThumbnailRenderer",
    "playlistCustomThumbnailRenderer",
    "heroPlaylistThumbnailRenderer",
    "playlistVideoListRenderer",
    "alertRenderer",
    "alertWithButtonRenderer",
    "expandableMetadataRenderer",
    "metadataRowContainerRenderer",
];

/// Content shapes that carry a result but are not `*Renderer` keys and have no mapping yet.
///
/// `shortsLockupViewModel` is a Shorts shelf item. The Phase 4.3 capture of a real first page
/// contains 25 of them (65 on the `veritasium` page): each one is a result the caller cannot see.
/// They were dropped without any diagnostic because the key does not end in `Renderer`, so
/// `parse_warnings` stayed quiet — the exact "unknown content silently disappears" failure this
/// phase is about. Counting them here makes the gap visible; mapping them is out of scope until
/// the video id can be read from the reel endpoint instead of being parsed out of an `entityId`
/// string (which is how a lookalike id would become a "valid" result).
pub const UNMAPPED_CONTENT_VIEW_MODELS: &[&str] = &["shortsLockupViewModel"];

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
    unmapped_results: u32,
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

    /// A content shape that carries a result but has no mapping (see [`UNMAPPED_CONTENT_VIEW_MODELS`]).
    ///
    /// Kept apart from [`Self::unknown_renderer`] because that counter also covers page chrome —
    /// dialogs, filter chips, menus, share panels — which a healthy page has hundreds of. Losing a
    /// *result* is a different class of event, and it is what `Strict` mode fails on.
    pub fn unmapped_result(&mut self, key: &str) {
        self.unmapped_results = self.unmapped_results.saturating_add(1);
        self.warn(format!("unmapped_content:{key}"));
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
            .saturating_add(self.unmapped_results)
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

    /// Number of results that were recognised as content but not mapped.
    pub fn unmapped_results(&self) -> u32 {
        self.unmapped_results
    }

    /// Number of content-level problems: dropped results, missing fields, structural surprises.
    ///
    /// This is the number `Strict` mode fails on; `unknown_renderers()` is deliberately not part of
    /// it (see [`Self::unmapped_result`]).
    pub fn content_problems(&self) -> u32 {
        self.unmapped_results
            .saturating_add(self.skipped_items)
            .saturating_add(self.missing_fields)
            .saturating_add(u32::from(self.structural))
    }

    pub fn has_structural_problem(&self) -> bool {
        self.structural
    }

    pub fn messages(&self) -> &[String] {
        &self.warnings
    }

    pub fn merge(&mut self, other: ParseReport) {
        self.unknown_renderers += other.unknown_renderers;
        self.unmapped_results += other.unmapped_results;
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
    ///
    /// Known limitation, measured in Phase 4.3: a *live* page carries page chrome (hotkey dialogs,
    /// filter chips, share panels, menu items) that is counted as `unknown_renderer` — 133
    /// occurrences on the captured first page — even though every result on it mapped. `Strict` is
    /// therefore only usable against reduced documents, which is exactly how the fixtures are built.
    /// [`Self::content_problems`] is the number that distinguishes "a result was lost" from "the
    /// page had buttons"; making `Strict` fail on that number instead is a Phase 4.4 decision, not
    /// something to change quietly here.
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
    let mut stack: Vec<(&Value, usize)> = vec![(root, 0)];
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
                // Children are pushed in reverse so that the LIFO stack pops them in document
                // order: search results must keep the order the upstream served them in.
                let mut children: Vec<(&Value, usize)> = Vec::new();
                for (key, value) in map {
                    // A content shape with no mapping: counted, and not descended into (its inner
                    // renderers all describe the same item, so walking them would multiply the
                    // count without adding information).
                    if UNMAPPED_CONTENT_VIEW_MODELS.contains(&key.as_str()) && !value.is_null() {
                        report.unmapped_result(key);
                        continue;
                    }
                    if key.ends_with("Renderer")
                        || key == "lockupViewModel"
                        || key == "videoRenderer"
                    {
                        if KNOWN_RENDERERS.contains(&key.as_str()) {
                            found.push(Renderer {
                                key: key.as_str(),
                                value,
                            });
                            // Do not descend into a consumed renderer: its inner renderers belong
                            // to a different resource (e.g. channelRenderer inside videoRenderer).
                            continue;
                        }
                        if key.ends_with("Renderer")
                            && !value.is_null()
                            && !STRUCTURAL_RENDERERS.contains(&key.as_str())
                        {
                            report.unknown_renderer(key);
                        }
                    }
                    if value.is_object() || value.is_array() {
                        children.push((value, depth + 1));
                    }
                }
                stack.extend(children.into_iter().rev());
            }
            Value::Array(items) => {
                for item in items.iter().rev() {
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

/// Find the token that continues the *page*, not a filter chip.
///
/// A search response carries two families of tokens:
///
/// * the page cursor, on `continuationItemRenderer.continuationEndpoint.continuationCommand`;
/// * one token per filter chip, on `chipCloudChipRenderer.navigationEndpoint.continuationCommand`.
///
/// They are different operations: a chip token asks upstream for a *filtered or re-sorted* result set.
/// Phase 4.3 measured the difference on a live page — the walk below returned the chip token
/// (812 chars) while the page cursor was 638 chars, so "next page" would have silently changed the
/// result set instead of paging it (that is also why the captured "page 2" is a chip continuation, see
/// `docs/research/SEARCH_RESPONSE_ANALYSIS.md`). The cursor is therefore taken from the item renderer
/// first; any other `continuationCommand` is a fallback for older envelopes that hang the token off
/// the section itself.
pub fn find_continuation(root: &Value) -> Option<String> {
    find_page_cursor(root).or_else(|| find_any_continuation_token(root))
}

/// `continuationItemRenderer.continuationEndpoint.continuationCommand.token`.
///
/// Public since Phase 6 because channel and playlist pages need the *page* cursor only: their filter
/// chips and sort menus also carry `continuationCommand` tokens (they re-sort or re-filter the same
/// list), and treating one of those as "next page" silently changes the result set instead of paging it.
pub fn find_page_cursor(root: &Value) -> Option<String> {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return None;
        }
        match node {
            Value::Object(map) => {
                let token = map
                    .get("continuationItemRenderer")
                    .and_then(|item| item.get("continuationEndpoint"))
                    .and_then(|endpoint| endpoint.get("continuationCommand"))
                    .and_then(|command| command.get("token"))
                    .and_then(Value::as_str)
                    .filter(|token| !token.is_empty());
                if let Some(token) = token {
                    return Some(token.to_owned());
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

/// Any `continuationCommand.token`, wherever it is (fallback path).
fn find_any_continuation_token(root: &Value) -> Option<String> {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES {
            return None;
        }
        match node {
            Value::Object(map) => {
                if let Some(token) = map
                    .get("continuationCommand")
                    .and_then(|command| command.get("token"))
                    .and_then(Value::as_str)
                    && !token.is_empty()
                {
                    return Some(token.to_owned());
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
    while index < bytes.len()
        && (bytes[index].is_ascii_whitespace() || bytes[index] == b'=' || bytes[index] == b':')
    {
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
                                        {"channelRenderer": {"channelId": "UCBv7HEHvY6Y0a0dS0R5mQ0w"}},
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
        assert!(
            report
                .messages()
                .iter()
                .any(|m| m.contains("unknownThingRenderer"))
        );
    }

    #[test]
    fn depth_budget_stops_pathological_nesting() {
        // A linear 200-deep chain is already four times `MAX_DEPTH`. The previous version fed
        // `value` into two siblings per iteration, so building the fixture itself was
        // exponential (2^200 nodes) and hung the test binary instead of the parser.
        let mut value = json!({"leaf": true});
        for _ in 0..200 {
            value = json!({ "nestedRendererDepth": { "child": value } });
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
