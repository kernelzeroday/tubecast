use anyhow::{bail, Result};
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Video(String),
    Playlist(String),
}

fn looks_like_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn looks_like_playlist_id(s: &str) -> bool {
    // Playlist ids are longer than a video id and carry a known prefix.
    s.len() > 11
        && ["PL", "RD", "UU", "LL", "FL", "OL", "RDCLAK", "WL"]
            .iter()
            .any(|p| s.starts_with(p))
}

/// Accepts full YouTube URLs (watch, youtu.be, shorts, embed, live, playlist),
/// bare video ids, or bare playlist ids.
pub fn parse_target(input: &str) -> Result<Target> {
    let input = input.trim();

    if let Ok(url) = Url::parse(input) {
        if url.scheme() == "http" || url.scheme() == "https" {
            return from_url(&url);
        }
    }

    if looks_like_playlist_id(input) {
        return Ok(Target::Playlist(input.to_string()));
    }
    if looks_like_video_id(input) {
        return Ok(Target::Video(input.to_string()));
    }
    bail!("could not recognize '{input}' as a YouTube URL, video id, or playlist id");
}

fn from_url(url: &Url) -> Result<Target> {
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|seg| !seg.is_empty()).collect())
        .unwrap_or_default();

    match host {
        "youtu.be" => {
            if let Some(id) = segments.first() {
                return Ok(Target::Video((*id).to_string()));
            }
        }
        "youtube.com" | "m.youtube.com" | "music.youtube.com" => match segments.first().copied() {
            Some("watch") => {
                if let Some(v) = query("v") {
                    return Ok(Target::Video(v));
                }
                if let Some(list) = query("list") {
                    return Ok(Target::Playlist(list));
                }
            }
            Some("playlist") => {
                if let Some(list) = query("list") {
                    return Ok(Target::Playlist(list));
                }
            }
            Some("shorts") | Some("embed") | Some("live") | Some("v") => {
                if let Some(id) = segments.get(1) {
                    return Ok(Target::Video((*id).to_string()));
                }
            }
            _ => {}
        },
        _ => {}
    }
    bail!("unsupported YouTube URL: {url}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let v = Target::Video("dQw4w9WgXcQ".into());
        assert_eq!(parse_target("dQw4w9WgXcQ").unwrap(), v);
        assert_eq!(parse_target("https://youtu.be/dQw4w9WgXcQ").unwrap(), v);
        assert_eq!(
            parse_target("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10s").unwrap(),
            v
        );
        assert_eq!(
            parse_target("https://www.youtube.com/shorts/dQw4w9WgXcQ").unwrap(),
            v
        );
        assert_eq!(
            parse_target("https://www.youtube.com/playlist?list=PLabc123def456").unwrap(),
            Target::Playlist("PLabc123def456".into())
        );
        // watch URL with both v and list prefers the single video
        assert_eq!(
            parse_target("https://youtube.com/watch?v=dQw4w9WgXcQ&list=PLxyz987654321").unwrap(),
            v
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_target("not a video").is_err());
    }
}
