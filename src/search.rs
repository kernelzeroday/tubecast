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
}

impl SearchResult {
    /// One-line label for pickers and listings.
    pub fn label(&self) -> String {
        let mut meta = Vec::new();
        if !self.author.is_empty() {
            meta.push(self.author.clone());
        }
        if !self.length.is_empty() {
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

/// Query an Invidious-style search API. Tolerates both the vanilla Invidious
/// shape (top-level array) and Playlet's invidious-companion shape
/// (`{ "items": [...] }`), and the differing field names between them.
pub async fn search(base: &str, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    let url = format!("{}/api/v1/search", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let resp = client
        .get(&url)
        .query(&[("q", query), ("type", "video")])
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

    // Playlet's invidious-companion shape: { items: [...] } with text fields.
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
        assert!(r.label().contains("TOOL"));
        assert!(r.label().contains("7 years ago"));
    }

    // Vanilla Invidious shape: numeric lengthSeconds / viewCount.
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
    }

    #[test]
    fn skips_non_videos() {
        let channel = serde_json::json!({ "type": "channel", "author": "TOOL" });
        assert!(parse_item(&channel).is_none());
    }
}
