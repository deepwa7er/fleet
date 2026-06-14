use std::net::IpAddr;
use std::path::PathBuf;

use serde::Deserialize;

/// Runtime configuration, loaded from a TOML file.
///
/// For local development, point [`Config::source_dir`] at a checkout of the
/// secondbrain repo and leave [`Config::git_remote`] unset — harbor just reads
/// the directory. On the VPS, set `git_remote` and harbor keeps `source_dir` in
/// sync with it.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address to bind. On the VPS this is the Tailscale IP, so the service is
    /// reachable only from the tailnet (the same model lighthouse uses).
    pub bind: IpAddr,

    /// TCP port to listen on.
    pub port: u16,

    /// Working copy of the secondbrain repo that harbor reads. If `git_remote`
    /// is set this directory is managed by harbor; otherwise it is read as-is.
    pub source_dir: PathBuf,

    /// Optional git remote. When set, harbor clones it into `source_dir` if
    /// missing and hard-resets to `origin/<branch>` on each refresh.
    #[serde(default)]
    pub git_remote: Option<String>,

    /// Branch to track when `git_remote` is set.
    #[serde(default = "default_branch")]
    pub branch: String,

    /// How often (seconds) to re-sync and re-parse the secondbrain.
    #[serde(default = "default_refresh_secs")]
    pub refresh_secs: u64,

    /// CORS allow-origin. `"*"` (the default) is appropriate for a read-only,
    /// tailnet-bound service; set a specific origin to lock it down.
    #[serde(default = "default_allow_origin")]
    pub allow_origin: String,
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_refresh_secs() -> u64 {
    300
}

fn default_allow_origin() -> String {
    "*".to_string()
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))?;
        Ok(config)
    }
}
