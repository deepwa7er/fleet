use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// The repo's example config doubles as the baseline written on first run, so
/// the documented defaults and the runtime baseline can never drift apart.
const BASELINE_CONFIG: &str = include_str!("../lighthouse.toml");

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Address to listen on — set to the VPS's Tailscale IP for tailnet-only access.
    pub bind: IpAddr,
    pub port: u16,
    /// Directory holding the built frontend served at `/`.
    pub static_dir: PathBuf,
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    /// systemd unit name, e.g. `notes.service`.
    pub unit: String,
    /// Human-friendly label shown in the UI.
    pub name: String,
}

impl Config {
    /// Load and validate the config from `path`, creating it from the embedded
    /// baseline if it does not yet exist.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating config directory {}", parent.display()))?;
            }
            std::fs::write(path, BASELINE_CONFIG)
                .with_context(|| format!("writing baseline config to {}", path.display()))?;
        }

        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;

        if config.services.is_empty() {
            bail!("config lists no services to monitor");
        }
        Ok(config)
    }

    /// Look up a configured service by its unit name. This is the allowlist that
    /// gates every systemd/journal command — names not present here are rejected
    /// before any subprocess runs.
    pub fn find_unit(&self, unit: &str) -> Option<&ServiceConfig> {
        self.services.iter().find(|s| s.unit == unit)
    }
}
