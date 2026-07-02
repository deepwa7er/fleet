//! `source.toml` — the service's runtime config. Small and host-shaped: where to
//! bind, which fleet manifest to read, and where the built frontend lives.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address to bind. On the dev box this is the host's Tailscale IP, so
    /// breakwater (on the VPS) can reach it over the tailnet. For local dev,
    /// `127.0.0.1`.
    pub bind: IpAddr,
    /// Port to listen on.
    pub port: u16,
    /// Path to the fleet manifest (`fleet.toml`) whose members are the repos to
    /// serve. `~/` expands to the home directory.
    #[serde(deserialize_with = "expand_tilde")]
    pub fleet: PathBuf,
    /// Directory holding the built frontend (`web/dist`). Served for every
    /// non-`/api` path, with an SPA fallback to its `index.html`. `~/` expands.
    #[serde(deserialize_with = "expand_tilde")]
    pub web_dir: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(config)
    }
}

/// Expand a leading `~/` to the user's home directory, leaving other paths as-is.
/// Keeps `source.toml` portable across machines (the dev box and fedora differ).
fn expand_tilde<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(crate::fleet::expand_tilde(&raw))
}
