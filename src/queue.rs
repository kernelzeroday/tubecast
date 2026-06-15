use anyhow::Result;
use std::time::Duration;

use crate::config::Config;
use crate::search;
use crate::transport::get_now_playing;

pub async fn queue(device: Option<&str>, timeout: u64) -> Result<()> {
    let cfg = Config::load()?;
    let dev = cfg.resolve(device)?;

    if let Some(web) = &dev.web_url {
        if let Some(q) = playlet_queue(web).await {
            let search_base = dev.search_base(&cfg);
            print_playlet_queue(&q, search_base.as_deref()).await;
            return Ok(());
        }
    }

    let remote_id = get_now_playing(&cfg, device, timeout).await;
    match remote_id {
        Some(id) => {
            let title = match dev.search_base(&cfg) {
                Some(base) => search::video_title(&base, &id).await,
                None => None,
            };
            if let Some(t) = title {
                println!("{t}");
            }
            println!("playing https://youtu.be/{id}");
            println!("(full queue only available for Playlet devices linked with `link-web`)");
        }
        None => println!("nothing playing (or TV not responding)"),
    }
    Ok(())
}

struct PlayletQueue {
    index: usize,
    items: Vec<String>,
}

async fn playlet_queue(web: &str) -> Option<PlayletQueue> {
    let url = format!("{}/api/queue", web.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let items: Vec<String> = json
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("videoid").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return None;
    }
    Some(PlayletQueue { index, items })
}

async fn print_playlet_queue(q: &PlayletQueue, search_base: Option<&str>) {
    for (i, id) in q.items.iter().enumerate() {
        let marker = if i == q.index { " >" } else { "  " };
        let title = match search_base {
            Some(base) => search::video_title(base, id).await,
            None => None,
        };
        match title {
            Some(t) => println!("{marker} {i}  {t}  (https://youtu.be/{id})"),
            None => println!("{marker} {i}  https://youtu.be/{id}"),
        }
    }
}
