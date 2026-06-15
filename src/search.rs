use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub length: String,
    pub views: String,
    pub published: String,
    pub is_live: bool,
}

impl SearchResult {
    /// One-line label for pickers and listings.
    pub fn label(&self) -> String {
        let mut meta = Vec::new();
        if !self.author.is_empty() {
            meta.push(self.author.clone());
        }
        if self.is_live {
            meta.push("LIVE".to_string());
        } else if !self.length.is_empty() {
            meta.push(self.length.clone());
        }
        if !self.views.is_empty() {
            meta.push(self.views.clone());
        }
        if !self.published.is_empty() {
            meta.push(self.published.clone());
        }
        if meta.is_empty() {
            self.title.clone()
        } else {
            format!("{}  —  {}", self.title, meta.join(" · "))
        }
    }
}

#[derive(Debug, Default)]
pub struct SearchFilter {
    pub live: bool,
    pub sort_by: Option<String>,
}

/// Query an Invidious-style search API. Tolerates both the vanilla Invidious
/// shape (top-level array) and Playlet's invidious-companion shape
/// (`{ "items": [...] }`), and the differing field names between them.
pub async fn search(base: &str, query: &str, limit: usize, page: usize, filter: &SearchFilter) -> Result<Vec<SearchResult>> {
    let url = format!("{}/api/v1/search", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let sort_param = filter.sort_by.as_deref().unwrap_or("");
    let page_str = page.to_string();
    let mut params: Vec<(&str, &str)> = vec![("q", query), ("type", "video"), ("page", &page_str)];
    if filter.live {
        params.push(("features", "live"));
    }
    if !sort_param.is_empty() {
        params.push(("sort_by", sort_param));
    }
    let resp = client
        .get(&url)
        .query(&params)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;

    if !resp.status().is_success() {
        bail!("search backend returned HTTP {}", resp.status());
    }

    let body: Value = resp.json().await.context("parsing search response")?;
    let items = match body {
        Value::Array(a) => a,
        Value::Object(mut o) => o
            .remove("items")
            .and_then(|v| match v {
                Value::Array(a) => Some(a),
                _ => None,
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let mut out = Vec::new();
    for item in &items {
        if let Some(r) = parse_item(item) {
            out.push(r);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Fetch the device's on-device search history. Playlet exposes this at
/// `/api/search-history` (on the web root, not the invidious backend) as a
/// JSON array of query strings, most-recent first.
pub async fn search_history(web_base: &str) -> Result<Vec<String>> {
    let url = format!("{}/api/search-history", web_base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    if !resp.status().is_success() {
        bail!("device returned HTTP {} for search history", resp.status());
    }
    resp.json().await.context("parsing search history")
}

/// Fetch a single video's title from an Invidious-style backend. Best-effort:
/// returns None on any network or parse failure, so callers can degrade
/// gracefully when no metadata backend is reachable.
pub async fn video_title(base: &str, video_id: &str) -> Option<String> {
    let url = format!("{}/api/v1/videos/{}", base.trim_end_matches('/'), video_id);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let title = body.get("title").and_then(Value::as_str)?.trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn parse_item(item: &Value) -> Option<SearchResult> {
    // Only videos: must have a videoId and not be a channel/playlist entry.
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "channel" || kind == "playlist" || kind == "shelf" {
        return None;
    }
    let video_id = item.get("videoId").and_then(Value::as_str)?.to_string();
    if video_id.is_empty() {
        return None;
    }
    let is_live = item.get("liveNow").and_then(Value::as_bool).unwrap_or(false);
    let title = str_field(item, "title");
    let author = str_field(item, "author");
    let length = length_field(item);
    let views = views_field(item);
    let published = published_field(item);
    Some(SearchResult {
        video_id,
        title,
        author,
        length,
        views,
        published,
        is_live,
    })
}

fn str_field(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn length_field(item: &Value) -> String {
    if let Some(text) = item.get("lengthText").and_then(Value::as_str) {
        if !text.is_empty() {
            return text.to_string();
        }
    }
    match item.get("lengthSeconds").and_then(Value::as_i64) {
        Some(secs) if secs > 0 => fmt_duration(secs),
        _ => String::new(),
    }
}

fn views_field(item: &Value) -> String {
    if let Some(text) = item.get("viewCountText").and_then(Value::as_str) {
        if !text.is_empty() {
            return text.to_string();
        }
    }
    match item.get("viewCount").and_then(Value::as_i64) {
        Some(n) if n > 0 => format!("{} views", human_count(n)),
        _ => String::new(),
    }
}

fn published_field(item: &Value) -> String {
    if let Some(text) = item.get("publishedText").and_then(Value::as_str) {
        if !text.is_empty() {
            return text.to_string();
        }
    }
    match item.get("published").and_then(Value::as_i64) {
        Some(ts) if ts > 0 => fmt_relative_ts(ts),
        _ => String::new(),
    }
}

fn fmt_relative_ts(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now - unix;
    if diff < 0 {
        return String::new();
    }
    let days = diff / 86400;
    if days < 1 {
        "today".to_string()
    } else if days < 30 {
        format!("{days} days ago")
    } else if days < 365 {
        let months = days / 30;
        if months == 1 { "1 month ago".to_string() } else { format!("{months} months ago") }
    } else {
        let years = days / 365;
        if years == 1 { "1 year ago".to_string() } else { format!("{years} years ago") }
    }
}

fn fmt_duration(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn human_count(n: i64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.1}K", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_companion_shape() {
        let item = serde_json::json!({
            "type": "video",
            "videoId": "MM62wjLrgmA",
            "title": "TOOL - Schism (Official Video)",
            "author": "TOOL",
            "lengthText": "7:27",
            "viewCountText": "72M views",
            "publishedText": "7 years ago"
        });
        let r = parse_item(&item).unwrap();
        assert_eq!(r.video_id, "MM62wjLrgmA");
        assert_eq!(r.length, "7:27");
        assert_eq!(r.views, "72M views");
        assert_eq!(r.published, "7 years ago");
        assert!(!r.is_live);
        assert!(r.label().contains("TOOL"));
        assert!(r.label().contains("7 years ago"));
    }

    #[test]
    fn parses_vanilla_shape() {
        let item = serde_json::json!({
            "type": "video",
            "videoId": "dQw4w9WgXcQ",
            "title": "Never Gonna Give You Up",
            "author": "Rick Astley",
            "lengthSeconds": 213,
            "viewCount": 1_600_000_000_i64,
            "published": 572227200_i64
        });
        let r = parse_item(&item).unwrap();
        assert_eq!(r.length, "3:33");
        assert_eq!(r.views, "1.6B views");
        assert!(r.published.contains("years ago"));
        assert!(!r.is_live);
    }

    #[test]
    fn detects_live_video() {
        let item = serde_json::json!({
            "type": "video",
            "videoId": "livetest12x",
            "title": "Live Stream",
            "author": "Streamer",
            "liveNow": true,
            "viewCountText": "1.2K watching"
        });
        let r = parse_item(&item).unwrap();
        assert!(r.is_live);
        assert!(r.label().contains("LIVE"));
        assert!(!r.label().contains("· ·"));
    }

    #[test]
    fn live_label_replaces_length() {
        let r = SearchResult {
            video_id: "x".into(),
            title: "Stream".into(),
            author: "A".into(),
            length: "0:00".into(),
            views: "1K".into(),
            published: "".into(),
            is_live: true,
        };
        let l = r.label();
        assert!(l.contains("LIVE"));
        assert!(!l.contains("0:00"));
    }

    #[test]
    fn label_title_only_when_no_meta() {
        let r = SearchResult {
            video_id: "x".into(),
            title: "Title".into(),
            author: "".into(),
            length: "".into(),
            views: "".into(),
            published: "".into(),
            is_live: false,
        };
        assert_eq!(r.label(), "Title");
    }

    #[test]
    fn parse_item_missing_optional_fields() {
        let item = serde_json::json!({ "videoId": "abcdefghijk" });
        let r = parse_item(&item).unwrap();
        assert_eq!(r.video_id, "abcdefghijk");
        assert_eq!(r.title, "");
        assert_eq!(r.author, "");
        assert_eq!(r.length, "");
        assert_eq!(r.views, "");
        assert!(!r.is_live);
    }

    #[test]
    fn parse_item_empty_video_id() {
        let item = serde_json::json!({ "type": "video", "videoId": "" });
        assert!(parse_item(&item).is_none());
    }

    #[test]
    fn parse_item_no_video_id() {
        let item = serde_json::json!({ "type": "video", "title": "No ID" });
        assert!(parse_item(&item).is_none());
    }

    #[test]
    fn skips_all_non_video_types() {
        for kind in &["channel", "playlist", "shelf"] {
            let item = serde_json::json!({ "type": kind, "videoId": "abcdefghijk" });
            assert!(parse_item(&item).is_none(), "should skip type={kind}");
        }
    }

    #[test]
    fn fmt_duration_edge_cases() {
        assert_eq!(fmt_duration(0), "0:00");
        assert_eq!(fmt_duration(1), "0:01");
        assert_eq!(fmt_duration(59), "0:59");
        assert_eq!(fmt_duration(60), "1:00");
        assert_eq!(fmt_duration(3599), "59:59");
        assert_eq!(fmt_duration(3600), "1:00:00");
        assert_eq!(fmt_duration(3661), "1:01:01");
    }

    #[test]
    fn human_count_boundaries() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1_000), "1.0K");
        assert_eq!(human_count(1_500), "1.5K");
        assert_eq!(human_count(1_000_000), "1.0M");
        assert_eq!(human_count(1_000_000_000), "1.0B");
    }

    proptest! {
        #[test]
        fn fmt_duration_never_empty(secs in 0i64..1_000_000) {
            prop_assert!(!fmt_duration(secs).is_empty());
            let s = fmt_duration(secs);
            prop_assert!(s.contains(':'), "expected colon in '{s}'");
        }

        #[test]
        fn human_count_never_empty(n in 0i64..i64::MAX) {
            prop_assert!(!human_count(n).is_empty());
        }

        #[test]
        fn parse_item_never_panics(
            vid in "[a-zA-Z0-9_-]{0,15}",
            title in "[a-zA-Z0-9 ]{0,30}",
            live in proptest::bool::ANY,
            secs in 0i64..100_000,
            kind in prop::sample::select(vec!["video", "channel", "playlist", "shelf", ""]),
        ) {
            let mut obj = serde_json::json!({
                "title": title,
                "lengthSeconds": secs,
                "liveNow": live,
            });
            if !vid.is_empty() {
                obj["videoId"] = serde_json::json!(vid);
            }
            if !kind.is_empty() {
                obj["type"] = serde_json::json!(kind);
            }
            let _ = parse_item(&obj);
        }
    }
}
