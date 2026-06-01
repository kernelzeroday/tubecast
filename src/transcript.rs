use anyhow::{bail, Context, Result};

use crate::parse::{parse_target, Target};

pub fn transcript(target: &str, lang: &str) -> Result<()> {
    let video_id = match parse_target(target)? {
        Target::Video(id) => id,
        Target::Playlist(_) => bail!("`transcript` requires a single video, not a playlist"),
    };

    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let out_stem = std::env::temp_dir().join(format!("tubecast_sub_{video_id}"));
    let out_template = out_stem.to_string_lossy().to_string();

    let status = std::process::Command::new("yt-dlp")
        .args([
            "--skip-download",
            "--write-auto-subs",
            "--sub-lang",
            lang,
            "--sub-format",
            "vtt",
            "-o",
            &out_template,
            &url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("yt-dlp not found; install with `brew install yt-dlp` or `pip install yt-dlp`")?;

    let vtt_path = format!("{out_template}.{lang}.vtt");
    if !status.success() {
        let _ = std::fs::remove_file(&vtt_path);
        bail!("yt-dlp failed (video may have no captions for lang '{lang}')");
    }
    let vtt = std::fs::read_to_string(&vtt_path)
        .with_context(|| format!("subtitle file not found: {vtt_path}"))?;
    let _ = std::fs::remove_file(&vtt_path);

    println!("{}", parse_vtt(&vtt));
    Ok(())
}

fn parse_vtt(vtt: &str) -> String {
    let mut cues: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut in_cue = false;

    for line in vtt.lines() {
        let line = line.trim();
        if line.contains("-->") {
            if !current.is_empty() {
                cues.push(current.clone());
                current.clear();
            }
            in_cue = true;
            continue;
        }
        if line.is_empty() {
            in_cue = false;
            continue;
        }
        if line.starts_with("WEBVTT") || line.starts_with("NOTE") || line.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if in_cue {
            let s = strip_vtt_tags(line);
            if !s.is_empty() {
                current.push(s);
            }
        }
    }
    if !current.is_empty() {
        cues.push(current);
    }

    // YouTube auto-subs use a rolling window: cue N repeats lines from cue N-1.
    // Only emit lines that are new relative to the previous cue.
    let mut out: Vec<String> = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for cue in cues {
        for line in &cue {
            if !prev.contains(line) {
                out.push(line.clone());
            }
        }
        prev = cue;
    }
    out.join("\n")
}

fn strip_vtt_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_plain_text_unchanged() {
        assert_eq!(strip_vtt_tags("hello world"), "hello world");
    }

    #[test]
    fn strip_c_tags() {
        assert_eq!(strip_vtt_tags("<c>hello</c>"), "hello");
    }

    #[test]
    fn strip_timestamp_tags() {
        assert_eq!(strip_vtt_tags("<00:00:01.000><c>hello</c>"), "hello");
    }

    #[test]
    fn strip_mixed_inline_tags() {
        assert_eq!(
            strip_vtt_tags("Hello <00:00:01.920><c> world</c>"),
            "Hello  world"
        );
    }

    #[test]
    fn strip_trims_whitespace() {
        assert_eq!(strip_vtt_tags("  <c>text</c>  "), "text");
    }

    #[test]
    fn strip_empty_after_tags() {
        assert_eq!(strip_vtt_tags("<c></c>"), "");
    }

    #[test]
    fn parse_skips_header_and_timestamps() {
        let vtt = "WEBVTT\nKind: captions\n\n\
                   00:00:01.000 --> 00:00:03.000\nhello world\n\n";
        assert_eq!(parse_vtt(vtt), "hello world");
    }

    #[test]
    fn parse_strips_inline_tags() {
        let vtt = "WEBVTT\n\n\
                   00:00:01.000 --> 00:00:03.000\n<00:00:01.000><c>hello</c>\n\n";
        assert_eq!(parse_vtt(vtt), "hello");
    }

    #[test]
    fn parse_deduplicates_rolling_window() {
        let vtt = "WEBVTT\n\n\
                   00:00:01.000 --> 00:00:02.000\nfirst line\n\n\
                   00:00:02.000 --> 00:00:03.000\nfirst line\nsecond line\n\n\
                   00:00:03.000 --> 00:00:04.000\nsecond line\nthird line\n\n";
        assert_eq!(parse_vtt(vtt), "first line\nsecond line\nthird line");
    }

    #[test]
    fn parse_skips_note_blocks() {
        let vtt = "WEBVTT\n\nNOTE\nsome metadata\n\n\
                   00:00:01.000 --> 00:00:02.000\nhello\n\n";
        assert_eq!(parse_vtt(vtt), "hello");
    }

    #[test]
    fn parse_skips_cue_sequence_numbers() {
        let vtt = "WEBVTT\n\n\
                   1\n00:00:01.000 --> 00:00:02.000\nfoo\n\n\
                   2\n00:00:02.000 --> 00:00:03.000\nfoo\nbar\n\n";
        assert_eq!(parse_vtt(vtt), "foo\nbar");
    }

    #[test]
    fn parse_empty_vtt() {
        assert_eq!(parse_vtt("WEBVTT\n\n"), "");
    }
}
