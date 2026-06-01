use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio::time::{sleep, Instant};
use youtube_lounge_rs::{LoungeClient, LoungeError, LoungeEvent, PlaybackStatus};

use crate::config::Config;

pub const DEVICE_NAME: &str = "tubecast";

pub const UNPAIRED_HINT: &str =
    "this device is no longer paired — the TV dropped the link. On the TV, open the \
     YouTube/Playlet app, find \"Link with TV code\" in settings, then re-pair with \
     `tubecast pair <CODE>`.";

/// Whether the paired screen can currently be reached.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// Screen is reachable and the token is valid (possibly just refreshed).
    Available,
    /// Token is valid but the screen isn't available (TV off or app closed).
    Unavailable,
    /// The pairing is dead; a fresh `tubecast pair <CODE>` is required.
    Unpaired,
}

#[derive(Clone, Copy)]
pub enum Action {
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
    pub fn label(self) -> String {
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

    pub async fn dispatch(self, client: &LoungeClient) -> Result<()> {
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

    pub fn confirmed_by(self, ev: &LoungeEvent) -> bool {
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

pub fn build_client(cfg: &Config, device: Option<&str>) -> Result<LoungeClient> {
    let dev = cfg.resolve(device)?;
    let client = LoungeClient::new(&dev.screen_id, &dev.lounge_token, DEVICE_NAME, None, None);
    Ok(client)
}

pub async fn connect_ready(client: &LoungeClient) -> Result<()> {
    client
        .set_token_refresh_callback(Config::update_token)
        .await;
    if let Err(e) = client.connect_with_refresh().await {
        return Err(connect_error(e));
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = format!("{:?}", client.current_state());
        if state == "Connected" {
            return Ok(());
        }
        if let Some(reason) = state.strip_prefix("Failed") {
            if looks_unpaired(reason) {
                bail!("{UNPAIRED_HINT}");
            }
            bail!("connection failed: {state}");
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for the screen to connect");
        }
        sleep(Duration::from_millis(150)).await;
    }
}

/// A 401 (token) or 404 (screen forgotten) after auto-refresh means the
/// pairing is gone and a fresh code is needed.
fn looks_unpaired(s: &str) -> bool {
    s.contains("Token") || s.contains("token") || s.contains("404") || s.contains("ot found")
}

fn connect_error(e: LoungeError) -> anyhow::Error {
    match e {
        LoungeError::TokenExpired | LoungeError::TokenRefreshFailed(_) => anyhow!("{UNPAIRED_HINT}"),
        LoungeError::InvalidResponse(m) if looks_unpaired(&m) => anyhow!("{UNPAIRED_HINT}"),
        other => anyhow::Error::new(other)
            .context("connecting to screen (is the TV on with Playlet/YouTube open?)"),
    }
}

/// Check whether the paired screen is reachable without opening a full
/// session. Auto-refreshes the lounge token (and persists it) if expired.
pub async fn probe_screen(cfg: &Config, device: Option<&str>) -> Result<Reachability> {
    let client = build_client(cfg, device)?;
    client
        .set_token_refresh_callback(Config::update_token)
        .await;
    match client.check_screen_availability_with_refresh().await {
        Ok(true) => Ok(Reachability::Available),
        Ok(false) => Ok(Reachability::Unavailable),
        Err(LoungeError::TokenExpired | LoungeError::TokenRefreshFailed(_)) => {
            Ok(Reachability::Unpaired)
        }
        Err(LoungeError::InvalidResponse(m)) if looks_unpaired(&m) => Ok(Reachability::Unpaired),
        Err(e) => Err(e).context("checking whether the screen is reachable"),
    }
}

pub async fn finish(client: &LoungeClient) {
    sleep(Duration::from_millis(300)).await;
    let _ = client.disconnect().await;
}

pub fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>) {
    while let Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) = rx.try_recv()
    {}
}

pub async fn wait_confirm(
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

pub struct Playback {
    pub video_id: String,
    pub status: PlaybackStatus,
}

pub enum CastResult {
    /// The screen confirmed it's playing the requested video.
    Confirmed(PlaybackStatus),
    /// The screen is still playing a different video; the cast didn't take.
    OtherPlaying(String),
    /// No playback update arrived at all.
    Unconfirmed,
}

/// Send `play_video` and wait for the screen to confirm, resending once if the
/// first command goes unacknowledged (the bind channel can drop a command sent
/// immediately after connecting — the same issue handled in `wait_confirm`).
pub async fn play_video_confirmed(
    client: &LoungeClient,
    rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>,
    video_id: &str,
) -> Result<CastResult> {
    let mut other: Option<String> = None;
    for (attempt, wait_ms) in [(0u8, 2500u64), (1, 5000)] {
        let what = if attempt == 0 { "send play" } else { "resend play" };
        client
            .play_video(video_id.to_string())
            .await
            .context(what)?;
        match wait_playing(rx, Duration::from_millis(wait_ms)).await {
            Some(pb) if pb.video_id == video_id => return Ok(CastResult::Confirmed(pb.status)),
            Some(pb) => other = Some(pb.video_id),
            None => {}
        }
    }
    Ok(other.map_or(CastResult::Unconfirmed, CastResult::OtherPlaying))
}

/// Wait for the screen to report active playback, returning what it's playing.
pub async fn wait_playing(
    rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>,
    dur: Duration,
) -> Option<Playback> {
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(LoungeEvent::NowPlaying(np))) if !np.video_id.is_empty() => {
                return Some(Playback {
                    status: np.status(),
                    video_id: np.video_id,
                });
            }
            Ok(Ok(LoungeEvent::PlaybackSession(s))) if !s.video_id.is_empty() => {
                return Some(Playback {
                    status: s.status(),
                    video_id: s.video_id,
                });
            }
            Ok(Ok(_)) => continue,
            _ => return None,
        }
    }
}

/// Wait for the screen to acknowledge a queue change.
pub async fn wait_queued(
    rx: &mut tokio::sync::broadcast::Receiver<LoungeEvent>,
    dur: Duration,
) -> bool {
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(LoungeEvent::PlaylistModified(_))) => return true,
            Ok(Ok(_)) => continue,
            _ => return false,
        }
    }
}

pub async fn get_now_playing(cfg: &Config, device: Option<&str>, timeout: u64) -> Option<String> {
    let client = build_client(cfg, device).ok()?;
    let mut rx = client.event_receiver();
    connect_ready(&client).await.ok()?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let found = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(LoungeEvent::NowPlaying(np))) if !np.video_id.is_empty() => {
                break Some(np.video_id);
            }
            Ok(Ok(LoungeEvent::PlaybackSession(s))) if !s.video_id.is_empty() => {
                break Some(s.video_id);
            }
            Ok(Ok(_)) => continue,
            _ => break None,
        }
    };
    finish(&client).await;
    found
}

pub async fn replay_queue(
    cfg: &Config,
    device: Option<&str>,
    current: &str,
    tail: &[String],
) -> Result<()> {
    let client = build_client(cfg, device)?;
    connect_ready(&client).await?;
    client
        .play_video(current.to_string())
        .await
        .context("send play")?;
    finish(&client).await;

    if !tail.is_empty() {
        let client = build_client(cfg, device)?;
        connect_ready(&client).await?;
        for id in tail {
            client
                .add_video_to_queue(id.clone())
                .await
                .context("send add")?;
        }
        finish(&client).await;
    }
    Ok(())
}
