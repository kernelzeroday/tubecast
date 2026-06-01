mod config;
mod parse;
mod search;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use config::{Config, Device, LocalQueue};
use dialoguer::{theme::ColorfulTheme, FuzzySelect};
use parse::{parse_target, Target};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use youtube_lounge_rs::{LoungeClient, LoungeEvent, PlaybackStatus};

const DEVICE_NAME: &str = "tubecast";

#[derive(Parser)]
#[command(
    name = "tubecast",
    version,
    about = "Cast YouTube videos to Playlet / YouTube TV apps over the Lounge API"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct DeviceArg {
    /// Target device alias (defaults to the configured default or only device)
    #[arg(short, long)]
    device: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Pair with a TV using the code from "Link with TV code"
    Pair {
        /// The pairing code shown on the TV
        code: String,
        /// Friendly alias to refer to this device later
        #[arg(short, long)]
        alias: Option<String>,
        /// Make this the default device
        #[arg(long)]
        default: bool,
    },
    /// Cast a video or playlist, replacing current playback
    Play {
        /// YouTube URL, video id, or playlist id
        target: String,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Add a video to the queue
    Add {
        /// YouTube URL or video id
        target: String,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Resume playback
    Resume(DeviceArg),
    /// Pause playback
    Pause(DeviceArg),
    /// Skip to the next video
    Next(DeviceArg),
    /// Go to the previous video
    Prev(DeviceArg),
    /// Skip the current ad
    SkipAd(DeviceArg),
    /// Mute audio
    Mute(DeviceArg),
    /// Unmute audio
    Unmute(DeviceArg),
    /// Seek to a position, in seconds
    Seek {
        seconds: f64,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Set volume (0-100)
    Volume {
        level: i32,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Search YouTube and cast a result
    Search {
        /// Search terms
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Queue the choice instead of playing it now
        #[arg(short, long)]
        queue: bool,
        /// Skip the picker and cast the top result
        #[arg(long)]
        first: bool,
        /// Maximum number of results to show
        #[arg(short, long, default_value_t = 15)]
        limit: usize,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Show what is currently playing
    Status {
        #[command(flatten)]
        dev: DeviceArg,
        /// Seconds to wait for a status update
        #[arg(long, default_value_t = 8)]
        timeout: u64,
    },
    /// List paired devices
    Devices,
    /// Associate a device's web address (e.g. a Playlet TV) for keyless search
    LinkWeb {
        /// Base URL, e.g. http://192.168.1.209:8888
        url: String,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Shuffle the upcoming queue (reorders locally-tracked videos)
    Shuffle {
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Play a video immediately, pushing everything else back in the queue
    PushTop {
        /// YouTube URL or video id
        target: String,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Fetch and print the transcript for a video (requires yt-dlp)
    Transcript {
        /// YouTube URL or video id
        target: String,
        /// Subtitle language code
        #[arg(short, long, default_value = "en")]
        lang: String,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Pair {
            code,
            alias,
            default,
        } => pair(&code, alias, default).await,
        Command::Play { target, dev } => play(&target, dev.device.as_deref()).await,
        Command::Add { target, dev } => add(&target, dev.device.as_deref()).await,
        Command::Resume(d) => simple(d.device.as_deref(), Action::Resume).await,
        Command::Pause(d) => simple(d.device.as_deref(), Action::Pause).await,
        Command::Next(d) => simple(d.device.as_deref(), Action::Next).await,
        Command::Prev(d) => simple(d.device.as_deref(), Action::Prev).await,
        Command::SkipAd(d) => simple(d.device.as_deref(), Action::SkipAd).await,
        Command::Mute(d) => simple(d.device.as_deref(), Action::Mute).await,
        Command::Unmute(d) => simple(d.device.as_deref(), Action::Unmute).await,
        Command::Seek { seconds, dev } => {
            simple(dev.device.as_deref(), Action::Seek(seconds)).await
        }
        Command::Volume { level, dev } => {
            simple(dev.device.as_deref(), Action::Volume(level)).await
        }
        Command::Search {
            query,
            queue,
            first,
            limit,
            dev,
        } => run_search(&query.join(" "), dev.device.as_deref(), queue, first, limit).await,
        Command::Status { dev, timeout } => status(dev.device.as_deref(), timeout).await,
        Command::Devices => devices(),
        Command::LinkWeb { url, dev } => link_web(&url, dev.device.as_deref()),
        Command::Shuffle { dev } => shuffle(dev.device.as_deref()).await,
        Command::PushTop { target, dev } => push_top(&target, dev.device.as_deref()).await,
        Command::Transcript { target, lang } => transcript(&target, &lang),
    }
}

async fn pair(code: &str, alias: Option<String>, default: bool) -> Result<()> {
    let cleaned: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let screen = LoungeClient::pair_with_screen(&cleaned)
        .await
        .context("pairing failed (check the code on the TV and that it is still showing)")?;

    let name = screen.name.clone().unwrap_or_else(|| "TV".to_string());
    let alias = alias.unwrap_or_else(|| slugify(&name));

    let mut cfg = Config::load()?;
    let make_default = default || cfg.devices.is_empty();
    cfg.upsert(Device {
        alias: alias.clone(),
        name: name.clone(),
        screen_id: screen.screen_id,
        lounge_token: screen.lounge_token,
        web_url: None,
    });
    if make_default {
        cfg.default_device = Some(alias.clone());
    }
    cfg.save()?;

    println!(
        "paired '{name}' as '{alias}'{}",
        if make_default { " (default)" } else { "" }
    );
    Ok(())
}

async fn play(target: &str, device: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;
    match parse_target(target)? {
        Target::Video(id) => {
            client.play_video(id.clone()).await.context("send play")?;
            let _ = LocalQueue::reset(&id);
            println!("playing https://youtu.be/{id}");
        }
        Target::Playlist(list) => {
            client
                .play_playlist(list.clone())
                .await
                .context("send play")?;
            println!("playing playlist {list}");
        }
    }
    finish(&client).await;
    Ok(())
}

async fn add(target: &str, device: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;
    match parse_target(target)? {
        Target::Video(id) => {
            client
                .add_video_to_queue(id.clone())
                .await
                .context("send add")?;
            let _ = LocalQueue::push(&id);
            println!("queued https://youtu.be/{id}");
        }
        Target::Playlist(_) => bail!("`add` takes a single video; use `play` for a playlist"),
    }
    finish(&client).await;
    Ok(())
}

#[derive(Clone, Copy)]
enum Action {
    Resume,
    Pause,
    Next,
    Prev,
    SkipAd,
    Mute,
    Unmute,
    Seek(f64),
    Volume(i32),
}

impl Action {
    fn label(self) -> String {
        match self {
            Action::Resume => "resumed".into(),
            Action::Pause => "paused".into(),
            Action::Next => "next".into(),
            Action::Prev => "previous".into(),
            Action::SkipAd => "ad skipped".into(),
            Action::Mute => "muted".into(),
            Action::Unmute => "unmuted".into(),
            Action::Seek(s) => format!("seeked to {s}s"),
            Action::Volume(v) => format!("volume {v}"),
        }
    }

    async fn dispatch(self, client: &LoungeClient) -> Result<()> {
        match self {
            Action::Resume => client.play().await?,
            Action::Pause => client.pause().await?,
            Action::Next => client.next().await?,
            Action::Prev => client.previous().await?,
            Action::SkipAd => client.skip_ad().await?,
            Action::Mute => client.mute().await?,
            Action::Unmute => client.unmute().await?,
            Action::Seek(s) => client.seek_to(s).await?,
            Action::Volume(v) => client.set_volume(v).await?,
        }
        Ok(())
    }

    /// Does this event confirm the action took effect on the screen?
    fn confirmed_by(self, ev: &LoungeEvent) -> bool {
        use PlaybackStatus::{Buffering, Paused, Playing, Starting};
        let state_is = |ev: &LoungeEvent, want: &[PlaybackStatus]| match ev {
            LoungeEvent::StateChange(s) => want.contains(&s.status()),
            LoungeEvent::NowPlaying(n) => want.contains(&n.status()),
            _ => false,
        };
        match self {
            Action::Resume => state_is(ev, &[Playing, Starting, Buffering]),
            Action::Pause => state_is(ev, &[Paused]),
            Action::Mute | Action::Unmute | Action::Volume(_) => {
                matches!(ev, LoungeEvent::VolumeChanged(_))
            }
            Action::Next | Action::Prev | Action::SkipAd | Action::Seek(_) => {
                matches!(ev, LoungeEvent::StateChange(_) | LoungeEvent::NowPlaying(_))
            }
        }
    }
}

async fn simple(device: Option<&str>, action: Action) -> Result<()> {
    if let Action::Volume(v) = action {
        if !(0..=100).contains(&v) {
            bail!("volume must be 0-100");
        }
    }
    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await?;

    // Discard events buffered during connection so stale NowPlaying/StateChange
    // events don't falsely confirm the action before the TV sees it.
    drain_events(&mut rx);

    // The bind channel can drop a freshly-sent control command before the
    // screen acts on it, so send, wait for the screen to confirm the new
    // state, and resend once if the first attempt goes unacknowledged.
    action.dispatch(&client).await?;
    let mut confirmed = wait_confirm(&mut rx, action, Duration::from_millis(1500)).await;
    if !confirmed {
        action.dispatch(&client).await?;
        confirmed = wait_confirm(&mut rx, action, Duration::from_secs(3)).await;
    }

    println!(
        "{}{}",
        action.label(),
        if confirmed { "" } else { " (unconfirmed)" }
    );
    finish(&client).await;
    Ok(())
}

fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>) {
    loop {
        match rx.try_recv() {
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

async fn wait_confirm(
    rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>,
    action: Action,
    dur: Duration,
) -> bool {
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                if action.confirmed_by(&ev) {
                    return true;
                }
            }
            _ => return false,
        }
    }
}

async fn status(device: Option<&str>, timeout: u64) -> Result<()> {
    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await?;

    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            println!("no status received (nothing playing?)");
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(LoungeEvent::NowPlaying(np))) if !np.video_id.is_empty() => {
                let status = PlaybackStatus::from(np.state.as_str());
                println!(
                    "{status}: https://youtu.be/{} [{}/{}s]",
                    np.video_id,
                    fmt_secs(&np.current_time),
                    fmt_secs(&np.duration)
                );
                break;
            }
            Ok(Ok(LoungeEvent::PlaybackSession(s))) if !s.video_id.is_empty() => {
                println!(
                    "{}: https://youtu.be/{} [{:.0}/{:.0}s]",
                    s.status(),
                    s.video_id,
                    s.current_time,
                    s.duration
                );
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => {
                println!("no status received (nothing playing?)");
                break;
            }
        }
    }
    finish(&client).await;
    Ok(())
}

fn devices() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.devices.is_empty() {
        println!("no paired devices. Run `tubecast pair <CODE>`.");
        return Ok(());
    }
    for d in &cfg.devices {
        let is_default = cfg.default_device.as_deref() == Some(d.alias.as_str());
        println!(
            "{}{}\t{}",
            d.alias,
            if is_default { " (default)" } else { "" },
            d.name
        );
    }
    Ok(())
}

async fn run_search(
    query: &str,
    device: Option<&str>,
    queue: bool,
    first: bool,
    limit: usize,
) -> Result<()> {
    let cfg = Config::load()?;
    let base = {
        let dev = cfg.resolve(device)?;
        dev.search_base(&cfg).with_context(|| {
            format!(
                "no search backend for '{}'. Link a Playlet TV with \
                 `tubecast link-web http://<tv-ip>:8888`, or set search_instance \
                 in the config to an Invidious instance.",
                dev.alias
            )
        })?
    };

    let results = search::search(&base, query, limit).await?;
    if results.is_empty() {
        println!("no results for \"{query}\"");
        return Ok(());
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let idx = if first {
        0
    } else if !interactive {
        // Scriptable: emit "<videoId>\t<label>" and let the caller choose.
        for r in &results {
            println!("{}\t{}", r.video_id, r.label());
        }
        return Ok(());
    } else {
        let labels: Vec<String> = results.iter().map(search::SearchResult::label).collect();
        let prompt = if queue {
            "Queue which video?"
        } else {
            "Cast which video?"
        };
        match FuzzySelect::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .items(&labels)
            .default(0)
            .interact_opt()?
        {
            Some(i) => i,
            None => {
                println!("cancelled");
                return Ok(());
            }
        }
    };

    let chosen = &results[idx];
    cast_video(&cfg, device, &chosen.video_id, queue).await?;
    println!(
        "{} https://youtu.be/{}  ({})",
        if queue { "queued" } else { "playing" },
        chosen.video_id,
        chosen.title
    );
    Ok(())
}

async fn cast_video(cfg: &Config, device: Option<&str>, video_id: &str, queue: bool) -> Result<()> {
    let client = build_client(cfg, device)?;
    connect_ready(&client).await?;
    if queue {
        client.add_video_to_queue(video_id.to_string()).await?;
        let _ = LocalQueue::push(video_id);
    } else {
        client.play_video(video_id.to_string()).await?;
        let _ = LocalQueue::reset(video_id);
    }
    finish(&client).await;
    Ok(())
}

fn link_web(url: &str, device: Option<&str>) -> Result<()> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("url must start with http:// or https://");
    }
    let mut cfg = Config::load()?;
    let alias = cfg.resolve(device)?.alias.clone();
    let trimmed = url.trim_end_matches('/').to_string();
    if let Some(d) = cfg.device_mut(&alias) {
        d.web_url = Some(trimmed.clone());
    }
    cfg.save()?;
    println!("linked '{alias}' -> {trimmed} (search enabled)");
    Ok(())
}

fn build_client(cfg: &Config, device: Option<&str>) -> Result<LoungeClient> {
    let dev = cfg.resolve(device)?;
    let client = LoungeClient::new(&dev.screen_id, &dev.lounge_token, DEVICE_NAME, None, None);
    Ok(client)
}

/// connect_with_refresh returns before the background manager flips the state
/// to Connected; send_command errors until then, so wait for readiness.
async fn connect_ready(client: &LoungeClient) -> Result<()> {
    client
        .set_token_refresh_callback(Config::update_token)
        .await;
    client
        .connect_with_refresh()
        .await
        .context("connecting to screen (is the TV on with Playlet/YouTube open?)")?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = format!("{:?}", client.current_state());
        if state == "Connected" {
            return Ok(());
        }
        if state.starts_with("Failed") {
            bail!("connection failed: {state}");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for the screen to connect");
        }
        sleep(Duration::from_millis(150)).await;
    }
}

/// Give the command a moment to flush over the bind channel, then disconnect.
async fn finish(client: &LoungeClient) {
    sleep(Duration::from_millis(300)).await;
    let _ = client.disconnect().await;
}

async fn shuffle(device: Option<&str>) -> Result<()> {
    let mut q = LocalQueue::load()?;
    if q.video_ids.len() < 2 {
        bail!("nothing in the queue to shuffle");
    }

    // Fisher-Yates on the tail (keep the currently-playing video at index 0)
    let mut rng = Rng::new();
    let tail = &mut q.video_ids[1..];
    for i in (1..tail.len()).rev() {
        let j = rng.next() % (i + 1);
        tail.swap(i, j);
    }

    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;

    // Re-issue play for index 1 (first upcoming), then add the rest.
    // Index 0 is already playing so we leave it alone.
    let upcoming = q.video_ids[1..].to_vec();
    client
        .play_video(upcoming[0].clone())
        .await
        .context("send play for shuffled head")?;
    finish(&client).await;

    // Re-connect for the queue additions
    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;
    for id in &upcoming[1..] {
        client
            .add_video_to_queue(id.clone())
            .await
            .context("send add for shuffled video")?;
    }
    finish(&client).await;

    q.save()?;
    println!("shuffled {} upcoming videos", upcoming.len());
    Ok(())
}

async fn push_top(target: &str, device: Option<&str>) -> Result<()> {
    let video_id = match parse_target(target)? {
        Target::Video(id) => id,
        Target::Playlist(_) => bail!("`push-top` takes a single video, not a playlist"),
    };

    let mut q = LocalQueue::load()?;

    // Remove the target from wherever it sits (it may not be tracked yet).
    q.video_ids.retain(|id| id != &video_id);

    // Build the tail: everything that was in the queue, in order.
    // Index 0 was the playing video; it goes back to position 1 so it
    // resumes after the promoted video finishes.
    let tail = q.video_ids.clone();

    let cfg = Config::load()?;
    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;
    client.play_video(video_id.clone()).await.context("send play")?;
    finish(&client).await;

    let client = build_client(&cfg, device)?;
    connect_ready(&client).await?;
    for id in &tail {
        client.add_video_to_queue(id.clone()).await.context("send add")?;
    }
    finish(&client).await;

    // Persist: promoted video is now index 0, old queue follows.
    let mut new_ids = vec![video_id.clone()];
    new_ids.extend(tail);
    LocalQueue { video_ids: new_ids }.save()?;

    println!("playing https://youtu.be/{video_id}");
    Ok(())
}

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        std::thread::current().id().hash(&mut h);
        Self(h.finish())
    }

    fn next(&mut self) -> usize {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

fn transcript(target: &str, lang: &str) -> Result<()> {
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

    if !status.success() {
        bail!("yt-dlp failed (video may have no captions for lang '{lang}')");
    }

    let vtt_path = format!("{out_template}.{lang}.vtt");
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

fn slugify(s: &str) -> String {
    let lowered: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for c in lowered.chars() {
        if c == '-' {
            if !prev_dash {
                out.push(c);
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "tv".to_string()
    } else {
        out
    }
}

fn fmt_secs(s: &str) -> String {
    s.parse::<f64>()
        .map(|v| format!("{v:.0}"))
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- strip_vtt_tags ---

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

    // --- parse_vtt ---

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
        // YouTube pattern: cue 2 repeats line from cue 1 and adds new line.
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

    // --- slugify ---

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("My TV"), "my-tv");
    }

    #[test]
    fn slugify_collapses_dashes() {
        assert_eq!(slugify("Living Room TV!"), "living-room-tv");
        assert_eq!(slugify("foo!!bar"), "foo-bar");
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify("!!!"), "tv");
    }
}
