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
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_device: Option<String>,
    #[serde(default)]
    pub devices: Vec<Device>,
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
        serde_json::from_str(&text)
            .with_context(|| format!("parsing config at {}", path.display()))
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

    pub fn upsert(&mut self, device: Device) {
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|d| d.alias.eq_ignore_ascii_case(&device.alias))
        {
            *existing = device;
        } else {
            self.devices.push(device);
        }
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
