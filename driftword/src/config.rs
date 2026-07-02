//! Runtime configuration for `driftword serve`, loaded from a TOML file.
//!
//! On the VPS `bind` is the Tailscale IP, so the service is reachable only from
//! the tailnet (the same model harbor and lighthouse use — the tailnet is the
//! security boundary).

use std::net::IpAddr;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address to bind. On the VPS this is the Tailscale IP.
    pub bind: IpAddr,
    /// TCP port to listen on.
    pub port: u16,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("parsing config {}: {e}", path.display()))
    }
}
