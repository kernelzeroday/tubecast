use anyhow::{bail, Result};

use crate::config::{Config, LocalQueue};
use crate::parse::{parse_target, Target};
use crate::transport::{get_now_playing, replay_queue};

fn check_drift(q: &LocalQueue, remote_id: &Option<String>) -> Result<()> {
    if let Some(rid) = remote_id {
        if let Some(lid) = q.video_ids.first() {
            if rid != lid {
                bail!(
                    "local queue is out of sync: TV is playing {rid} but local \
                     tracks {lid}. Run `tubecast play` or `tubecast push-top` to resync, \
                     or `tubecast queue` to inspect."
                );
            }
        }
    }
    Ok(())
}

pub async fn queue(device: Option<&str>, timeout: u64) -> Result<()> {
    let q = LocalQueue::load()?;
    let cfg = Config::load()?;
    let remote_id = get_now_playing(&cfg, device, timeout).await;

    match &remote_id {
        Some(id) => println!("remote: https://youtu.be/{id}"),
        None => println!("remote: unknown (TV not responding)"),
    }

    if q.video_ids.is_empty() {
        println!("local:  (empty)");
        return Ok(());
    }

    println!("local:");
    let local_current = q.video_ids.first().map(String::as_str);
    for (i, id) in q.video_ids.iter().enumerate() {
        let marker = if i == 0 { " (playing)" } else { "" };
        println!("  {i}  https://youtu.be/{id}{marker}");
    }

    if let (Some(rid), Some(lid)) = (&remote_id, local_current) {
        if rid != lid {
            println!("\nwarning: remote is playing {rid} but local thinks {lid} — run `push-top` or `play` to resync");
        }
    }
    Ok(())
}

pub async fn queue_remove(index: usize, device: Option<&str>) -> Result<()> {
    let mut q = LocalQueue::load()?;
    if q.video_ids.is_empty() {
        bail!("local queue is empty");
    }
    if index == 0 {
        bail!("index 0 is the currently playing video; use `play` to change it");
    }
    if index >= q.video_ids.len() {
        bail!("index {index} out of range (queue has {} entries)", q.video_ids.len());
    }

    let cfg = Config::load()?;
    let remote_id = get_now_playing(&cfg, device, 5).await;
    check_drift(&q, &remote_id)?;

    let removed = q.video_ids.remove(index);
    let current = q.video_ids[0].clone();
    let tail = q.video_ids[1..].to_vec();

    replay_queue(&cfg, device, &current, &tail).await?;
    q.save()?;
    println!("removed [{index}] https://youtu.be/{removed}");
    Ok(())
}

pub async fn queue_clear(device: Option<&str>) -> Result<()> {
    let mut q = LocalQueue::load()?;
    if q.video_ids.len() <= 1 {
        println!("queue is already empty");
        return Ok(());
    }

    let cfg = Config::load()?;
    let remote_id = get_now_playing(&cfg, device, 5).await;
    check_drift(&q, &remote_id)?;

    let current = q.video_ids[0].clone();
    let removed = q.video_ids.len() - 1;

    replay_queue(&cfg, device, &current, &[]).await?;
    q.video_ids.truncate(1);
    q.save()?;
    println!("cleared {removed} upcoming videos (still playing https://youtu.be/{current})");
    Ok(())
}

pub async fn shuffle(device: Option<&str>) -> Result<()> {
    let mut q = LocalQueue::load()?;
    if q.video_ids.len() < 2 {
        bail!("nothing in the queue to shuffle");
    }

    let cfg = Config::load()?;
    let remote_id = get_now_playing(&cfg, device, 5).await;
    check_drift(&q, &remote_id)?;

    let mut rng = Rng::new();
    let tail = &mut q.video_ids[1..];
    for i in (1..tail.len()).rev() {
        let j = rng.below(i + 1);
        tail.swap(i, j);
    }

    let current = q.video_ids[0].clone();
    let upcoming = q.video_ids[1..].to_vec();

    replay_queue(&cfg, device, &current, &upcoming).await?;
    q.save()?;
    println!("shuffled {} upcoming videos", upcoming.len());
    Ok(())
}

pub async fn push_top(target: &str, device: Option<&str>) -> Result<()> {
    let video_id = match parse_target(target)? {
        Target::Video(id) => id,
        Target::Playlist(_) => bail!("`push-top` takes a single video, not a playlist"),
    };

    let mut q = LocalQueue::load()?;
    q.video_ids.retain(|id| id != &video_id);
    let tail = q.video_ids.clone();

    let cfg = Config::load()?;
    replay_queue(&cfg, device, &video_id, &tail).await?;

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

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        let n = n as u64;
        let threshold = n.wrapping_neg() % n;
        loop {
            let r = self.next_u64();
            if r >= threshold {
                return (r % n) as usize;
            }
        }
    }
}
