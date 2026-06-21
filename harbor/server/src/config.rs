use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A persistent link to an external tailnet service, shown on the new-tab page
/// as a launchable card (distinct from secondbrain project/area notes). Declared
/// in the config so the extension stays a dumb view — it renders whatever links
/// the server provides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalLink {
    pub name: String,
    pub url: String,
}

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

    /// Optional GitHub token for fetching per-project commit activity. Public
    /// repos work without it; private repos need a token with read access.
    /// When unset, only public repos get activity.
    #[serde(default)]
    pub github_token: Option<String>,

    /// How long (seconds) a fetched commit-activity entry stays fresh before it
    /// is re-fetched. Decoupled from `refresh_secs` to respect GitHub rate
    /// limits — commit activity changes slowly.
    #[serde(default = "default_activity_ttl_secs")]
    pub activity_ttl_secs: u64,

    /// Optional lighthouse base URL (e.g. `http://100.98.184.58:8080`). When set,
    /// harbor fetches `/api/services` and includes live VPS service health.
    #[serde(default)]
    pub lighthouse_url: Option<String>,

    /// The fleet docs' generated `fleet.json`, read for the fleet-scale source
    /// stat (total lines of code) shown on the new-tab page. Best-effort: if the
    /// file is absent the stat is simply omitted.
    #[serde(default = "default_fleet_json_path")]
    pub fleet_json_path: PathBuf,

    /// CORS allow-origin. `"*"` (the default) is appropriate for a read-only,
    /// tailnet-bound service; set a specific origin to lock it down.
    #[serde(default = "default_allow_origin")]
    pub allow_origin: String,

    /// Persistent links to other tailnet services, rendered as launchable cards
    /// on the new-tab page (e.g. the driftword generator).
    #[serde(default)]
    pub external_links: Vec<ExternalLink>,

    /// Directory of self-hosted browser-extension artifacts (`harbor.crx` +
    /// `updates.xml`), served at `/extension`. Relative to the working
    /// directory (`/srv/harbor` on the VPS → `/srv/harbor/dist`). This is how
    /// the harbor extension is distributed + auto-updated to tailnet devices.
    #[serde(default = "default_extension_dir")]
    pub extension_dir: PathBuf,
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_refresh_secs() -> u64 {
    300
}

fn default_activity_ttl_secs() -> u64 {
    600
}

fn default_allow_origin() -> String {
    "*".to_string()
}

fn default_extension_dir() -> PathBuf {
    PathBuf::from("dist")
}

fn default_fleet_json_path() -> PathBuf {
    PathBuf::from("/opt/pilot/web/fleet.json")
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
