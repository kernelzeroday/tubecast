mod config;
mod dial;
mod parse;
mod queue;
mod search;
mod transcript;
mod transport;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use config::{Config, Device};
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

#[derive(Clone, Copy, clap::ValueEnum)]
enum Sort {
    /// Most relevant first (default)
    Relevance,
    /// Most recent first
    Date,
    /// Most viewed first
    Views,
    /// Highest rated first
    Rating,
}

impl Sort {
    fn as_api_param(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Date => "upload_date",
            Sort::Views => "view_count",
            Sort::Rating => "rating",
        }
    }
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
        /// Pick a random result instead of showing the picker
        #[arg(long)]
        random: bool,
        /// Only show live streams
        #[arg(long)]
        live: bool,
        /// Sort order for results
        #[arg(long, value_enum)]
        sort: Option<Sort>,
        /// Maximum number of results to show
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Results page (1 = first page)
        #[arg(short, long, default_value_t = 1)]
        page: usize,
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
    /// Choose the default device used when --device is omitted
    #[command(visible_alias = "set-default")]
    Use {
        /// Alias of the device to make default (omit to pick interactively)
        alias: Option<String>,
    },
    /// Associate a device's web address (e.g. a Playlet TV) for keyless search
    LinkWeb {
        /// Base URL, e.g. http://192.168.1.209:8888
        url: String,
        #[command(flatten)]
        dev: DeviceArg,
    },
    /// Discover YouTube-capable devices on the LAN (no pairing code needed)
    #[command(visible_alias = "scan")]
    Discover,
    /// Show the history the device exposes (Playlet: search history)
    History(DeviceArg),
    /// Show the queue (reads directly from Playlet devices)
    Queue {
        #[command(flatten)]
        dev: DeviceArg,
        /// Seconds to wait for a status update from the TV
        #[arg(long, default_value_t = 8)]
        timeout: u64,
    },
    /// You know what this does
    Rickroll(DeviceArg),
    /// Cast a random result for a search (feeling lucky)
    #[command(visible_alias = "random")]
    Lucky {
        /// Search terms (omit to use a random dictionary word)
        #[arg(num_args = 0..)]
        query: Vec<String>,
        /// Queue the choice instead of playing it now
        #[arg(short, long)]
        queue: bool,
        /// Only match live streams
        #[arg(long)]
        live: bool,
        /// Sort order for results
        #[arg(long, value_enum)]
        sort: Option<Sort>,
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
    ctrlc::set_handler(move || {
        let _ = dialoguer::console::Term::stderr().show_cursor();
        std::process::exit(130);
    })
    .ok();

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
            random,
            live,
            sort,
            limit,
            page,
            dev,
        } => {
            let filter = search::SearchFilter {
                live,
                sort_by: sort.map(|s| s.as_api_param().to_string()),
            };
            run_search(&query.join(" "), dev.device.as_deref(), queue, first, random, limit, page, &filter).await
        }
        Command::Status { dev, timeout } => status(dev.device.as_deref(), timeout).await,
        Command::Devices => devices().await,
        Command::Use { alias } => use_device(alias.as_deref()),
        Command::LinkWeb { url, dev } => link_web(&url, dev.device.as_deref()),
        Command::Discover => discover_devices().await,
        Command::History(d) => history(d.device.as_deref()).await,
        Command::Queue { dev, timeout } => queue::queue(dev.device.as_deref(), timeout).await,
        Command::Rickroll(d) => {
            let cfg = Config::load()?;
            cast(&cfg, d.device.as_deref(), "dQw4w9WgXcQ", false, "").await
        }
        Command::Lucky {
            query,
            queue,
            live,
            sort,
            dev,
        } => {
            let q = if query.is_empty() {
                let word = random_dict_word()?;
                eprintln!("searching for \"{word}\"");
                word
            } else {
                query.join(" ")
            };
            run_lucky(&q, dev.device.as_deref(), queue, live, sort).await
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

/// Cast (play or queue) a single video. For Playlet devices (those with a
/// web_url), use the direct web API which is faster and doesn't depend on
/// the YouTube lounge relay. For other devices (SmartTube, native YouTube),
/// use the lounge protocol with DIAL re-pair as a fallback.
async fn cast(
    cfg: &Config,
    device: Option<&str>,
    video_id: &str,
    queue: bool,
    suffix: &str,
) -> Result<()> {
    let web_url = cfg.resolve(device).ok().and_then(|d| d.web_url.clone());

    // Playlet: use the direct web API — faster and doesn't need the lounge
    if let Some(web) = &web_url {
        if playlet_cast(web, video_id, queue, suffix).await {
            return Ok(());
        }
    }

    // Lounge path (SmartTube / native YouTube, or Playlet web API unreachable)
    if cast_once(cfg, device, video_id, queue, suffix).await? {
        return Ok(());
    }

    // Lounge cast failed — try DIAL re-pair + retry
    match probe_screen(cfg, device).await {
        Ok(Reachability::Unavailable) => {
            if try_repair(cfg, device).await? {
                let cfg = Config::load()?;
                tokio::time::sleep(Duration::from_secs(1)).await;
                cast_once(&cfg, device, video_id, queue, suffix).await?;
            } else {
                eprintln!("make sure the TV is on with the YouTube/Playlet app open.");
            }
        }
        Ok(Reachability::Available | Reachability::Unpaired) => {
            if try_repair(cfg, device).await? {
                let cfg = Config::load()?;
                tokio::time::sleep(Duration::from_secs(1)).await;
                cast_once(&cfg, device, video_id, queue, suffix).await?;
            }
        }
        Err(_) => {}
    }
    Ok(())
}

/// Cast via the Playlet web API (POST /api/queue or /api/queue/play).
/// Returns true if the request succeeded.
async fn playlet_cast(web: &str, video_id: &str, queue: bool, suffix: &str) -> bool {
    let base = web.trim_end_matches('/');
    let url = if queue {
        format!("{base}/api/queue")
    } else {
        format!("{base}/api/queue/play")
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let body = serde_json::json!({ "videoId": video_id });
    match client.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => {
            let verb = if queue { "queued" } else { "playing" };
            println!("{verb} https://youtu.be/{video_id}{suffix} (via web API)");
            true
        }
        _ => false,
    }
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
        let ok = wait_queued(&mut rx, CONFIRM_WAIT).await;
        println!(
            "queued https://youtu.be/{video_id}{suffix}{}",
            if ok { "" } else { " (unconfirmed)" }
        );
        ok
    } else {
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

/// Try to re-pair a device whose cast session has gone stale. First attempts
/// DIAL auto-discovery on the LAN (works without user interaction), then falls
/// back to prompting for a manual TV code.
async fn try_repair(cfg: &Config, device: Option<&str>) -> Result<bool> {
    let dev = cfg.resolve(device)?;
    let alias = dev.alias.clone();
    let old_screen_id = dev.screen_id.clone();
    let dev_name = dev.name.clone();
    let web_ip = dev.web_url.as_ref().and_then(|u| ip_from_url(u));

    eprintln!("attempting LAN discovery…");
    if let Ok(found) = dial::discover(Duration::from_secs(4)).await {
        for d in &found {
            let dial_ip = ip_from_url(&d.location);
            let matched = d.screen_id == old_screen_id
                || slugify(&d.name) == alias
                || slugify(&d.name) == slugify(&dev_name)
                || (web_ip.is_some() && web_ip == dial_ip);
            if matched {
                let mut cfg = Config::load()?;
                if let Some(stored) = cfg.device_mut(&alias) {
                    stored.screen_id = d.screen_id.clone();
                    stored.lounge_token = d.lounge_token.clone();
                }
                cfg.save()?;
                eprintln!("re-paired '{}' via LAN discovery — retrying…", d.name);
                return Ok(true);
            }
        }
        if !found.is_empty() {
            eprintln!("found {} device(s) but none matched '{alias}'.", found.len());
        }
    }

    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!("note: {}", transport::UNPAIRED_HINT);
        eprintln!("tip: try `tubecast discover` to find devices on the network.");
        return Ok(false);
    }
    eprintln!(
        "LAN discovery didn't find a match — the TV may be off, or the app \
         isn't in the foreground."
    );
    let code: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Enter a fresh code from the TV (blank to skip)")
        .allow_empty(true)
        .interact_text()?;
    let code = code.trim();
    if code.is_empty() {
        eprintln!("skipped — try `tubecast discover` or `tubecast pair <CODE>`.");
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
    let web_url = cfg.resolve(device).ok().and_then(|d| d.web_url.clone());
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

    if confirmed {
        println!("{}", action.label());
    } else if let Some(web) = &web_url {
        if playlet_action(web, action).await {
            println!("{} (via web API)", action.label());
        } else {
            println!("{} (unconfirmed)", action.label());
        }
    } else {
        println!("{} (unconfirmed)", action.label());
    }
    finish(&client).await;
    Ok(())
}

async fn status(device: Option<&str>, timeout: u64) -> Result<()> {
    let cfg = Config::load()?;
    let dev_label = cfg
        .resolve(device)
        .map(|d| d.name.clone())
        .unwrap_or_else(|_| "the screen".to_string());

    let now = status_via_lounge(&cfg, device, timeout).await;

    match now {
        Some((status, video_id, cur, dur)) => {
            let title = match metadata_base(&cfg, device) {
                Some(base) => search::video_title(&base, &video_id).await,
                None => None,
            };
            if let Some(t) = title {
                println!("{t}");
            }
            println!("{status} on {dev_label} — https://youtu.be/{video_id} [{cur}/{dur}s]");
        }
        None => {
            let web_url = cfg.resolve(device).ok().and_then(|d| d.web_url.clone());
            match web_url.as_deref().and_then(|w| Some((w, metadata_base(&cfg, device)?))) {
                Some((web, base)) => {
                    if let Some((video_id, title)) = playlet_now_playing(web, &base).await {
                        if let Some(t) = &title {
                            println!("{t}");
                        }
                        println!("playing on {dev_label} — https://youtu.be/{video_id} (via web API)");
                    } else {
                        print_idle(&dev_label);
                    }
                }
                _ => print_idle(&dev_label),
            }
        }
    }
    Ok(())
}

async fn status_via_lounge(
    cfg: &Config,
    device: Option<&str>,
    timeout: u64,
) -> Option<(String, String, String, String)> {
    let client = build_client(cfg, device).ok()?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await.ok()?;

    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut now = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(LoungeEvent::NowPlaying(np))) if !np.video_id.is_empty() => {
                now = Some((
                    PlaybackStatus::from(np.state.as_str()).to_string(),
                    np.video_id,
                    fmt_secs(&np.current_time),
                    fmt_secs(&np.duration),
                ));
                break;
            }
            Ok(Ok(LoungeEvent::PlaybackSession(s))) if !s.video_id.is_empty() => {
                now = Some((
                    s.status().to_string(),
                    s.video_id,
                    format!("{:.0}", s.current_time),
                    format!("{:.0}", s.duration),
                ));
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    finish(&client).await;
    now
}

/// Pick an Invidious-style backend for looking up video metadata. Metadata is
/// keyed by video id, not by device, so any linked web backend works even when
/// the active device (e.g. SmartTube) has none of its own.
fn metadata_base(cfg: &Config, device: Option<&str>) -> Option<String> {
    if let Ok(d) = cfg.resolve(device) {
        if let Some(b) = d.search_base(cfg) {
            return Some(b);
        }
    }
    cfg.devices.iter().find_map(|d| d.search_base(cfg))
}

fn print_idle(dev_label: &str) {
    println!("connected to {dev_label}, but nothing is playing.");
    println!("(if you just switched apps on the TV, playback in the old app stopped — cast again with `tubecast play <video>`)");
}

async fn discover_devices() -> Result<()> {
    eprintln!("scanning for YouTube devices on the network…");
    let found = dial::discover(Duration::from_secs(5)).await?;
    if found.is_empty() {
        println!("no YouTube-capable devices found.");
        println!("make sure the TV is on with the YouTube/Playlet app open.");
        return Ok(());
    }

    let mut cfg = Config::load()?;
    for dev in &found {
        let existing = cfg
            .devices
            .iter()
            .find(|d| {
                d.screen_id == dev.screen_id
                    || slugify(&d.name) == slugify(&dev.name)
                    || d.web_url
                        .as_ref()
                        .and_then(|u| ip_from_url(u))
                        .is_some_and(|ip| ip_from_url(&dev.location) == Some(ip))
            })
            .map(|d| d.alias.clone());

        if let Some(alias) = existing {
            if let Some(d) = cfg.device_mut(&alias) {
                d.screen_id = dev.screen_id.clone();
                d.lounge_token = dev.lounge_token.clone();
            }
            println!("  refreshed '{alias}' ({})", dev.name);
        } else {
            let alias = slugify(&dev.name);
            let make_default = cfg.devices.is_empty();
            cfg.upsert(Device {
                alias: alias.clone(),
                name: dev.name.clone(),
                screen_id: dev.screen_id.clone(),
                lounge_token: dev.lounge_token.clone(),
                web_url: None,
            });
            if make_default {
                cfg.default_device = Some(alias.clone());
            }
            println!(
                "  paired '{alias}' ({}){}",
                dev.name,
                if make_default { " (default)" } else { "" }
            );
        }
    }
    cfg.save()?;
    Ok(())
}

async fn devices() -> Result<()> {
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
        if let Some(info) = device_info(d).await {
            println!("\t{info}");
        }
    }
    Ok(())
}

async fn device_info(d: &Device) -> Option<String> {
    let web = d.web_url.as_ref()?;
    let url = format!("{}/api/state", web.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let mut parts = Vec::new();
    if let Some(app) = json.get("app_version").and_then(|v| v.as_str()) {
        parts.push(format!("Playlet {app}"));
    }
    if let Some(model) = json.get("model_display_name").and_then(|v| v.as_str()) {
        parts.push(model.to_string());
    } else if let Some(model) = json.get("model_type").and_then(|v| v.as_str()) {
        parts.push(model.to_string());
    }
    if let Some(os) = json.get("os_version").and_then(|v| v.as_str()) {
        parts.push(format!("OS {os}"));
    }
    if parts.is_empty() { None } else { Some(parts.join(", ")) }
}

async fn playlet_action(web: &str, action: Action) -> bool {
    match action {
        Action::Next => playlet_navigate(web, 1).await,
        Action::Prev => playlet_navigate(web, -1).await,
        Action::Resume | Action::Pause => roku_keypress(web, "Play").await,
        Action::Mute => roku_keypress(web, "VolumeMute").await,
        Action::Unmute => roku_keypress(web, "VolumeMute").await,
        _ => false,
    }
}

async fn playlet_navigate(web: &str, offset: i64) -> bool {
    let base = web.trim_end_matches('/');
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match client
        .get(format!("{base}/api/queue"))
        .send()
        .await
        .and_then(|r| Ok(r))
    {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(j) => j,
            Err(_) => return false,
        },
        _ => return false,
    };
    let index = json.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
    let target = index + offset;
    let video_id = json
        .get("items")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(target as usize))
        .and_then(|item| item.get("videoid"))
        .and_then(|v| v.as_str());
    let Some(vid) = video_id else { return false };
    let body = serde_json::json!({ "videoId": vid });
    matches!(
        client.post(format!("{base}/api/queue/play")).json(&body).send().await,
        Ok(r) if r.status().is_success()
    )
}

async fn roku_keypress(web: &str, key: &str) -> bool {
    let host = match ip_from_url(web) {
        Some(h) => h,
        None => return false,
    };
    let url = format!("http://{host}:8060/keypress/{key}");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.post(&url).send().await, Ok(r) if r.status().is_success())
}

async fn playlet_now_playing(web: &str, search_base: &str) -> Option<(String, Option<String>)> {
    let url = format!("{}/api/queue", web.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    let video_id = json
        .get("nowplaying")
        .and_then(|np| np.get("videoid"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let title = search::video_title(search_base, &video_id).await;
    Some((video_id, title))
}

/// Set the default device, by alias or via an interactive picker.
fn use_device(alias: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    if cfg.devices.is_empty() {
        bail!("no paired devices. Run `tubecast pair <CODE>` first.");
    }
    let chosen = match alias {
        Some(a) => cfg.resolve(Some(a))?.alias.clone(),
        None => {
            if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
                bail!(
                    "pass a device alias (one of: {})",
                    cfg.devices
                        .iter()
                        .map(|d| d.alias.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            let labels: Vec<String> = cfg
                .devices
                .iter()
                .map(|d| format!("{}  ({})", d.alias, d.name))
                .collect();
            let start = cfg
                .default_device
                .as_ref()
                .and_then(|def| cfg.devices.iter().position(|d| &d.alias == def))
                .unwrap_or(0);
            match FuzzySelect::with_theme(&ColorfulTheme::default())
                .with_prompt("Default device")
                .items(&labels)
                .default(start)
                .interact_opt()?
            {
                Some(i) => cfg.devices[i].alias.clone(),
                None => {
                    println!("cancelled");
                    return Ok(());
                }
            }
        }
    };
    cfg.default_device = Some(chosen.clone());
    cfg.save()?;
    println!("default device is now '{chosen}'");
    Ok(())
}

async fn run_search(
    query: &str,
    device: Option<&str>,
    queue: bool,
    first: bool,
    random: bool,
    limit: usize,
    page: usize,
    filter: &search::SearchFilter,
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

    let results = search::search(&base, query, limit, page, filter).await?;
    if results.is_empty() {
        println!("no results for \"{query}\"");
        return Ok(());
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let idx = if first {
        0
    } else if random {
        random_index(results.len())
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
        let rows = dialoguer::console::Term::stderr().size().0 as usize;
        let max_visible = rows.saturating_sub(4).max(5);
        match FuzzySelect::with_theme(&ColorfulTheme::default())
            .with_prompt(prompt)
            .items(&labels)
            .max_length(max_visible)
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
    let suffix = if chosen.author.is_empty() {
        format!("  ({})", chosen.title)
    } else {
        format!("  ({} — {})", chosen.title, chosen.author)
    };
    cast(&cfg, device, &chosen.video_id, queue, &suffix).await?;
    Ok(())
}

const LUCKY_HISTORY_SIZE: usize = 100;

async fn run_lucky(
    query: &str,
    device: Option<&str>,
    queue: bool,
    live: bool,
    sort: Option<Sort>,
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

    let history = lucky_history_load();
    let fixed_sort = sort.map(|s| s.as_api_param().to_string());

    // Try up to 3 attempts with different sort/page combos
    for attempt in 0..3 {
        let sort_param = fixed_sort
            .clone()
            .unwrap_or_else(|| weighted_sort().to_string());
        let page = geometric_page();
        eprintln!("sort={sort_param} page={page}");
        let filter = search::SearchFilter {
            live,
            sort_by: Some(sort_param.clone()),
        };

        let results = search::search(&base, query, 20, page, &filter).await?;

        // Empty page — retry on page 1 before giving up
        if results.is_empty() {
            if page > 1 && attempt < 2 {
                continue;
            }
            let results = search::search(&base, query, 20, 1, &filter).await?;
            if results.is_empty() {
                println!("no results for \"{query}\"");
                return Ok(());
            }
            return pick_and_cast(&cfg, device, &results, &history, queue).await;
        }

        // Filter out recently played
        let fresh: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| !history.contains(&r.video_id))
            .map(|(i, _)| i)
            .collect();

        if fresh.is_empty() && attempt < 2 {
            continue;
        }

        let pool = if fresh.is_empty() { (0..results.len()).collect() } else { fresh };
        let idx = pool[random_index(pool.len())];
        let chosen = &results[idx];
        let suffix = if chosen.author.is_empty() {
            format!("  ({})", chosen.title)
        } else {
            format!("  ({} — {})", chosen.title, chosen.author)
        };
        lucky_history_push(&chosen.video_id);
        cast(&cfg, device, &chosen.video_id, queue, &suffix).await?;
        return Ok(());
    }

    println!("no results for \"{query}\"");
    Ok(())
}

async fn pick_and_cast(
    cfg: &Config,
    device: Option<&str>,
    results: &[search::SearchResult],
    history: &[String],
    queue: bool,
) -> Result<()> {
    let fresh: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| !history.contains(&r.video_id))
        .map(|(i, _)| i)
        .collect();
    let pool = if fresh.is_empty() { (0..results.len()).collect() } else { fresh };
    let idx = pool[random_index(pool.len())];
    let chosen = &results[idx];
    let suffix = if chosen.author.is_empty() {
        format!("  ({})", chosen.title)
    } else {
        format!("  ({} — {})", chosen.title, chosen.author)
    };
    lucky_history_push(&chosen.video_id);
    cast(cfg, device, &chosen.video_id, queue, &suffix).await
}

/// Weighted sort — de-emphasize relevance (YouTube's bubble) in favor of
/// sorts that surface different content.
fn weighted_sort() -> &'static str {
    // relevance=1, upload_date=3, view_count=3, rating=3 → 10% relevance, 30% each other
    let roll: u32 = rand::random_range(0..10);
    match roll {
        0 => "relevance",
        1..=3 => "upload_date",
        4..=6 => "view_count",
        _ => "rating",
    }
}

/// Geometric distribution for page selection — most picks land 1-5, but the
/// tail reaches deep into the index. Mean ≈ 5, but pages up to ~50 are
/// reachable.
fn geometric_page() -> usize {
    let u: f64 = rand::random_range(0.0f64..1.0f64);
    // geometric with p≈0.18 → mean ≈5.5, P(≤5)≈63%, P(≤10)≈87%, P(≤20)≈98%
    let page = (-u.ln() / 0.18_f64).floor() as usize + 1;
    page.min(50)
}

fn random_index(len: usize) -> usize {
    rand::random_range(0..len)
}

fn lucky_history_path() -> Option<std::path::PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "tubecast")?;
    Some(dirs.config_dir().join("lucky_history.json"))
}

fn lucky_history_load() -> Vec<String> {
    let Some(path) = lucky_history_path() else { return Vec::new() };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn lucky_history_push(video_id: &str) {
    let Some(path) = lucky_history_path() else { return };
    let mut hist = lucky_history_load();
    hist.retain(|id| id != video_id);
    hist.push(video_id.to_string());
    if hist.len() > LUCKY_HISTORY_SIZE {
        hist.drain(0..hist.len() - LUCKY_HISTORY_SIZE);
    }
    let _ = std::fs::write(&path, serde_json::to_string(&hist).unwrap_or_default());
}

fn random_dict_word() -> Result<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open("/usr/share/dict/words")
        .context("could not open /usr/share/dict/words")?;
    let lines: Vec<String> = BufReader::new(file)
        .lines()
        .filter_map(|l| {
            let w = l.ok()?;
            let w = w.trim().to_string();
            if w.len() >= 4 && w.chars().all(|c| c.is_ascii_alphabetic()) {
                Some(w.to_lowercase())
            } else {
                None
            }
        })
        .collect();
    if lines.is_empty() {
        bail!("no usable words in /usr/share/dict/words");
    }
    Ok(lines[random_index(lines.len())].clone())
}

/// Show whatever history the target device exposes over HTTP. Only Playlet TVs
/// (linked via `link-web`) expose anything, and only search history — watch
/// history isn't reachable without an Invidious login on the device.
async fn history(device: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let dev = cfg.resolve(device)?;
    let Some(web) = &dev.web_url else {
        bail!(
            "'{}' exposes no web API, so no history is available. Only Playlet TVs \
             (linked with `tubecast link-web http://<tv-ip>:8888`) expose history; \
             SmartTube and other apps don't.",
            dev.alias
        );
    };

    let terms = search::search_history(web).await?;
    if terms.is_empty() {
        println!("no search history on '{}'.", dev.alias);
        return Ok(());
    }
    println!("search history on {}:", dev.name);
    for t in &terms {
        println!("  {t}");
    }
    eprintln!(
        "note: this is search history; the device doesn't expose watch history over \
         HTTP (that needs an Invidious login, which isn't configured on the TV)."
    );
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

fn ip_from_url(u: &str) -> Option<String> {
    url::Url::parse(u).ok()?.host_str().map(|h| h.to_string())
}

fn fmt_secs(s: &str) -> String {
    s.parse::<f64>()
        .map(|v| format!("{v:.0}"))
        .unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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

    #[test]
    fn random_index_single() {
        assert_eq!(random_index(1), 0);
    }

    #[test]
    fn fmt_secs_formats_float() {
        assert_eq!(fmt_secs("123.456"), "123");
        assert_eq!(fmt_secs("0.0"), "0");
    }

    #[test]
    fn fmt_secs_passes_through_non_numeric() {
        assert_eq!(fmt_secs("abc"), "abc");
        assert_eq!(fmt_secs(""), "");
    }

    proptest! {
        #[test]
        fn random_index_always_in_bounds(len in 1usize..10_000) {
            prop_assert!(random_index(len) < len);
        }

        #[test]
        fn slugify_always_valid(s in ".{1,50}") {
            let r = slugify(&s);
            prop_assert!(!r.is_empty());
            prop_assert!(!r.starts_with('-'));
            prop_assert!(!r.ends_with('-'));
            prop_assert!(!r.contains("--"));
            prop_assert!(r.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        }
    }
}
