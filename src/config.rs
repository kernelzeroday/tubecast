use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub alias: String,
    pub name: String,
    pub screen_id: String,
    pub lounge_token: String,
    /// Base URL of the device's web server (e.g. a Playlet TV at
    /// http://192.168.1.209:8888), used as a no-key search backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_device: Option<String>,
    /// Fallback Invidious-style API base used for search when a device has no
    /// web_url (e.g. https://invidious.example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_instance: Option<String>,
    #[serde(default)]
    pub devices: Vec<Device>,
}

impl Device {
    /// Resolve the Invidious-style search API base for this device, falling
    /// back to a configured global instance.
    pub fn search_base(&self, cfg: &Config) -> Option<String> {
        if let Some(web) = &self.web_url {
            return Some(format!(
                "{}/playlet-invidious-backend",
                web.trim_end_matches('/')
            ));
        }
        cfg.search_instance
            .as_ref()
            .map(|s| s.trim_end_matches('/').to_string())
    }
}

/// Tracks the locally-known queue so `shuffle` can reorder it without
/// being able to read it back from the TV.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LocalQueue {
    pub video_ids: Vec<String>,
}

impl LocalQueue {
    fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "tubecast")
            .context("could not determine a config directory for this platform")?;
        Ok(dirs.config_dir().join("queue.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading queue at {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parsing queue at {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("writing queue at {}", path.display()))?;
        Ok(())
    }

    /// Replace the queue with a single video (called on `play`).
    pub fn reset(video_id: &str) -> Result<()> {
        Self { video_ids: vec![video_id.to_string()] }.save()
    }

    /// Append a video to the queue (called on `add`).
    pub fn push(video_id: &str) -> Result<()> {
        let mut q = Self::load()?;
        q.video_ids.push(video_id.to_string());
        q.save()
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "tubecast")
            .context("could not determine a config directory for this platform")?;
        Ok(dirs.config_dir().join("config.json"))
    }

    pub fn load() -> Result<Config> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing config at {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text)
            .with_context(|| format!("writing config at {}", path.display()))?;
        Ok(())
    }

    /// Resolve which device a command targets: explicit alias, else the
    /// configured default, else the only paired device.
    pub fn resolve(&self, alias: Option<&str>) -> Result<&Device> {
        if self.devices.is_empty() {
            bail!("no paired devices. Run `tubecast pair <CODE>` first.");
        }
        if let Some(alias) = alias {
            return self
                .devices
                .iter()
                .find(|d| d.alias.eq_ignore_ascii_case(alias))
                .with_context(|| format!("no paired device named '{alias}'"));
        }
        if let Some(def) = &self.default_device {
            if let Some(d) = self.devices.iter().find(|d| &d.alias == def) {
                return Ok(d);
            }
        }
        if self.devices.len() == 1 {
            return Ok(&self.devices[0]);
        }
        bail!(
            "multiple devices paired and no default set; pass --device <alias> (one of: {})",
            self.devices
                .iter()
                .map(|d| d.alias.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    pub fn upsert(&mut self, mut device: Device) {
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|d| d.alias.eq_ignore_ascii_case(&device.alias))
        {
            // Keep a previously-set web_url unless the new pairing supplies one.
            if device.web_url.is_none() {
                device.web_url = existing.web_url.take();
            }
            *existing = device;
        } else {
            self.devices.push(device);
        }
    }

    pub fn device_mut(&mut self, alias: &str) -> Option<&mut Device> {
        self.devices
            .iter_mut()
            .find(|d| d.alias.eq_ignore_ascii_case(alias))
    }

    /// Persist a refreshed lounge token for the device with this screen id.
    pub fn update_token(screen_id: &str, new_token: &str) {
        let Ok(mut cfg) = Config::load() else { return };
        let mut changed = false;
        for d in &mut cfg.devices {
            if d.screen_id == screen_id {
                d.lounge_token = new_token.to_string();
                changed = true;
            }
        }
        if changed {
            let _ = cfg.save();
        }
    }
}
