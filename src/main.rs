mod config;
mod parse;
mod queue;
mod search;
mod transcript;
mod transport;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use config::{Config, Device, LocalQueue};
use dialoguer::{theme::ColorfulTheme, FuzzySelect, Input};
use parse::{parse_target, Target};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::time::Instant;
use transport::{
    build_client, connect_ready, drain_events, finish, play_video_confirmed, probe_screen,
    wait_confirm, wait_queued, Action, CastResult, Reachability,
};
use youtube_lounge_rs::{LoungeClient, LoungeEvent, PlaybackStatus};

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
    /// Show the locally-tracked queue and what the TV is actually playing
    Queue {
        #[command(flatten)]
        dev: DeviceArg,
        /// Seconds to wait for a status update from the TV
        #[arg(long, default_value_t = 8)]
        timeout: u64,
    },
    /// Remove a video from the queue by index and resync the TV
    QueueRemove {
        /// Index to remove (use `queue` to see indices)
        index: usize,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Clear all upcoming videos from the queue (keeps current video playing)
    QueueClear {
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
        Command::Queue { dev, timeout } => queue::queue(dev.device.as_deref(), timeout).await,
        Command::QueueRemove { index, dev } => {
            queue::queue_remove(index, dev.device.as_deref()).await
        }
        Command::QueueClear { dev } => queue::queue_clear(dev.device.as_deref()).await,
        Command::Shuffle { dev } => queue::shuffle(dev.device.as_deref()).await,
        Command::PushTop { target, dev } => {
            queue::push_top(&target, dev.device.as_deref()).await
        }
        Command::Transcript { target, lang } => transcript::transcript(&target, &lang),
    }
}

/// Pair (or re-pair) a device from a TV code, upserting it into the config.
/// Returns the device's friendly name, alias, and whether it became default.
async fn do_pair(code: &str, alias: Option<String>, default: bool) -> Result<(String, String, bool)> {
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
    Ok((name, alias, make_default))
}

async fn pair(code: &str, alias: Option<String>, default: bool) -> Result<()> {
    let (name, alias, make_default) = do_pair(code, alias, default).await?;
    println!(
        "paired '{name}' as '{alias}'{}",
        if make_default { " (default)" } else { "" }
    );
    Ok(())
}

async fn play(target: &str, device: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    match parse_target(target)? {
        Target::Video(id) => cast(&cfg, device, &id, false, "").await,
        Target::Playlist(list) => {
            let client = build_client(&cfg, device)?;
            connect_ready(&client).await?;
            client
                .play_playlist(list.clone())
                .await
                .context("send play")?;
            if let Err(e) = LocalQueue::clear() {
                eprintln!("warning: could not clear local queue: {e}");
            }
            println!("playing playlist {list}");
            finish(&client).await;
            Ok(())
        }
    }
}

async fn add(target: &str, device: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    match parse_target(target)? {
        Target::Video(id) => cast(&cfg, device, &id, true, "").await,
        Target::Playlist(_) => bail!("`add` takes a single video; use `play` for a playlist"),
    }
}

/// How long to wait for the TV to confirm a queue change before probing.
const CONFIRM_WAIT: Duration = Duration::from_secs(6);

/// Cast (play or queue) a single video and confirm it took effect. If the TV
/// silently ignores the command — which happens once its cast session has
/// rotated (e.g. after switching apps on the TV, which gives Playlet a new
/// screen id and orphans the old pairing) — offer to re-pair with a fresh code
/// and retry automatically.
async fn cast(
    cfg: &Config,
    device: Option<&str>,
    video_id: &str,
    queue: bool,
    suffix: &str,
) -> Result<()> {
    if cast_once(cfg, device, video_id, queue, suffix).await? {
        return Ok(());
    }
    // The cast wasn't confirmed. A reachable-but-ignoring screen (stale session)
    // or an unpaired one can only be fixed by a fresh pairing code; a genuinely
    // unavailable screen just needs the TV/app brought up.
    match probe_screen(cfg, device).await {
        Ok(Reachability::Unavailable) => {
            eprintln!("note: the screen isn't responding — make sure the TV is on with the YouTube/Playlet app open and in the foreground.");
        }
        Ok(Reachability::Available | Reachability::Unpaired) => {
            if try_repair(cfg, device).await? {
                let cfg = Config::load()?; // re-pairing rewrote the stored screen id
                cast_once(&cfg, device, video_id, queue, suffix).await?;
            }
        }
        Err(_) => {}
    }
    Ok(())
}

/// A single cast attempt. Returns whether the TV confirmed it.
async fn cast_once(
    cfg: &Config,
    device: Option<&str>,
    video_id: &str,
    queue: bool,
    suffix: &str,
) -> Result<bool> {
    let client = build_client(cfg, device)?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await?;
    drain_events(&mut rx);

    let confirmed = if queue {
        client
            .add_video_to_queue(video_id.to_string())
            .await
            .context("send add")?;
        if let Err(e) = LocalQueue::push(video_id) {
            eprintln!("warning: could not update local queue: {e}");
        }
        // Not resent on failure: a missed ack would otherwise double-queue.
        let ok = wait_queued(&mut rx, CONFIRM_WAIT).await;
        println!(
            "queued https://youtu.be/{video_id}{suffix}{}",
            if ok { "" } else { " (unconfirmed)" }
        );
        ok
    } else {
        if let Err(e) = LocalQueue::reset(video_id) {
            eprintln!("warning: could not update local queue: {e}");
        }
        match play_video_confirmed(&client, &mut rx, video_id).await? {
            CastResult::Confirmed(status) => {
                println!("playing https://youtu.be/{video_id}{suffix} [{status}]");
                true
            }
            CastResult::OtherPlaying(other) => {
                println!("playing https://youtu.be/{video_id}{suffix} (unconfirmed)");
                eprintln!("note: the TV is still on https://youtu.be/{other} — the cast didn't take effect.");
                false
            }
            CastResult::Unconfirmed => {
                println!("playing https://youtu.be/{video_id}{suffix} (unconfirmed)");
                false
            }
        }
    };
    finish(&client).await;
    Ok(confirmed)
}

/// Prompt for a fresh TV code and re-pair the device in place. Returns whether
/// a re-pair happened, so the caller knows to retry. Non-interactive callers
/// just get the hint, since there's no way to read a code off the TV.
async fn try_repair(cfg: &Config, device: Option<&str>) -> Result<bool> {
    let alias = cfg.resolve(device)?.alias.clone();
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!("note: {}", transport::UNPAIRED_HINT);
        return Ok(false);
    }
    eprintln!(
        "the cast didn't take effect — the TV's cast session has changed \
         (common after switching apps on the TV), so the old pairing is stale."
    );
    let code: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter a fresh code from the TV's \"Link with TV code\" screen (blank to skip)")
        .allow_empty(true)
        .interact_text()?;
    let code = code.trim();
    if code.is_empty() {
        eprintln!("skipped — re-pair later with `tubecast pair <CODE>`.");
        return Ok(false);
    }
    let (name, _, _) = do_pair(code, Some(alias), false).await?;
    eprintln!("re-paired '{name}' — retrying…");
    Ok(true)
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

    drain_events(&mut rx);

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

async fn status(device: Option<&str>, timeout: u64) -> Result<()> {
    let cfg = Config::load()?;
    // connect_ready succeeding already proves the pairing is live, so an empty
    // result here means the screen is connected but idle, not unreachable.
    let dev_label = cfg
        .resolve(device)
        .map(|d| d.name.clone())
        .unwrap_or_else(|_| "the screen".to_string());
    let client = build_client(&cfg, device)?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await?;

    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            print_idle(&dev_label);
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
                print_idle(&dev_label);
                break;
            }
        }
    }
    finish(&client).await;
    Ok(())
}

fn print_idle(dev_label: &str) {
    println!("connected to {dev_label}, but nothing is playing.");
    println!("(if you just switched apps on the TV, playback in the old app stopped — cast again with `tubecast play <video>`)");
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
    let suffix = format!("  ({})", chosen.title);
    cast(&cfg, device, &chosen.video_id, queue, &suffix).await?;
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
