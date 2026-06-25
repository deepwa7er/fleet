//! Runtime configuration for `tide serve`, loaded from a TOML file.
//!
//! `bind` is loopback on the VPS; the breakwater reverse proxy is the tailnet
//! front door (the tailnet is the security boundary — no auth here). The settings
//! themselves persist under `data_dir`.

use std::net::IpAddr;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address to bind (loopback; breakwater fronts it).
    pub bind: IpAddr,
    /// TCP port to listen on.
    pub port: u16,
    /// Directory the settings file lives in (`<data_dir>/settings.json`).
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/tide")
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("parsing config {}: {e}", path.display()))
    }
}
