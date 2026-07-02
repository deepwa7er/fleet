use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

/// The repo's example config doubles as the baseline written on first run, so
/// the documented defaults and the runtime baseline can never drift apart.
const BASELINE_CONFIG: &str = include_str!("../lighthouse.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address to listen on — set to the VPS's Tailscale IP for tailnet-only access.
    pub bind: IpAddr,
    pub port: u16,
    /// Directory holding the built frontend served at `/`.
    pub static_dir: PathBuf,
    /// systemd target whose members are auto-discovered and monitored. A service
    /// joins the dashboard by becoming a dependency of this target (see README).
    #[serde(default = "default_target")]
    pub target: String,
    /// Optional override layer: rename a discovered service's label, or pin an
    /// extra unit that isn't a member of the target. Empty means pure discovery.
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Optional alerting. When present, a background watcher pushes a
    /// notification when a monitored service fails or crash-loops. Absent means
    /// no watcher runs.
    #[serde(default)]
    pub alerts: Option<AlertConfig>,
    /// Optional deploy integration. When present, the dashboard relays deploy
    /// requests to a tugboat `serve` daemon (typically on the dev machine,
    /// reached over the tailnet). Absent means no Deploy buttons appear.
    #[serde(default)]
    pub deploy: Option<DeployConfig>,
    /// The docs site's generated `fleet.json`, read to enrich each service with
    /// its description, public URL, and docs link (the fleet "home" view). Joined
    /// by unit name. Best-effort: if the file is absent the dashboard shows live
    /// status only, exactly as before.
    #[serde(default = "default_fleet_path")]
    pub fleet_path: PathBuf,
    /// Health-history collection. On by default; an absent `[history]` block uses
    /// the defaults, so it works without editing a deployed config.
    #[serde(default)]
    pub history: HistoryConfig,
}

/// Settings for the background health collector: how often it samples, how long
/// it keeps samples, and where it stores them. Sampling needs a writable data dir
/// — the systemd unit grants `/var/lib/lighthouse` via `StateDirectory=lighthouse`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// Set `false` to turn the collector (and the history API) off entirely.
    pub enabled: bool,
    /// SQLite database file for the time-series.
    pub db_path: PathBuf,
    /// Seconds between sample sweeps.
    pub interval_secs: u64,
    /// Days of history to retain; older samples are pruned each sweep.
    pub retention_days: i64,
    /// Per-service timeout (seconds) for the out-of-loopback reachability probe.
    pub probe_timeout_secs: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: PathBuf::from("/var/lib/lighthouse/history.db"),
            interval_secs: 60,
            retention_days: 30,
            probe_timeout_secs: 10,
        }
    }
}

fn default_fleet_path() -> PathBuf {
    PathBuf::from("/opt/pilot/web/fleet.json")
}

/// Deploy integration: where to reach the tugboat `serve` daemon.
#[derive(Debug, Clone, Deserialize)]
pub struct DeployConfig {
    /// Base URL of the tugboat daemon, e.g. `http://100.x.y.z:7878`.
    pub tugboat_url: String,
    /// Bearer token the daemon requires (its `TUGBOAT_SERVE_TOKEN`). Secret —
    /// keep this config root-readable only.
    pub token: String,
    /// Directory where tugboat writes per-service deploy stamps
    /// (`<deploy_name>.json`). These tell the dashboard which sha is running.
    #[serde(default = "default_version_dir")]
    pub version_dir: PathBuf,
}

fn default_version_dir() -> PathBuf {
    PathBuf::from("/var/lib/tugboat")
}

/// Alerting settings.
#[derive(Debug, Clone, Deserialize)]
pub struct AlertConfig {
    /// URL to POST notifications to (e.g. an ntfy topic
    /// `https://ntfy.sh/your-topic`). The body is the message; a `Title` header
    /// carries the service name.
    pub notify_url: String,
    /// How often (seconds) to poll service state.
    #[serde(default = "default_alert_interval")]
    pub interval_secs: u64,
}

fn default_alert_interval() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    /// systemd unit name, e.g. `notes.service`.
    pub unit: String,
    /// Display label override. If absent, the unit name is prettified.
    pub name: Option<String>,
    /// tugboat service name override for deploys. If absent, the unit's stem is
    /// used (`ferry.service` → `ferry`), which matches tugboat's fleet labels.
    pub deploy_name: Option<String>,
}

fn default_target() -> String {
    "lighthouse.target".to_owned()
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
        Ok(config)
    }

    /// Units pinned in the config that should be monitored regardless of target
    /// membership.
    pub fn pinned_units(&self) -> impl Iterator<Item = &str> {
        self.services.iter().map(|s| s.unit.as_str())
    }

    /// The display label for a unit: the config override if one is set, else the
    /// unit name prettified (`sonar-discovery.service` → `Sonar Discovery`).
    pub fn display_name(&self, unit: &str) -> String {
        self.services
            .iter()
            .find(|s| s.unit == unit)
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| prettify_unit(unit))
    }

    /// The tugboat service name a unit deploys as: the config override if set,
    /// else the unit's stem (`ferry.service` → `ferry`).
    pub fn deploy_name(&self, unit: &str) -> String {
        self.services
            .iter()
            .find(|s| s.unit == unit)
            .and_then(|s| s.deploy_name.clone())
            .unwrap_or_else(|| unit_stem(unit).to_owned())
    }
}

/// The unit name without its `.service` (or other) suffix.
fn unit_stem(unit: &str) -> &str {
    unit.rsplit_once('.').map(|(stem, _ext)| stem).unwrap_or(unit)
}

/// Turn a unit name into a human label: drop the `.service` suffix and
/// title-case the words separated by `-`/`_`. `sonar-discovery.service` becomes
/// `Sonar Discovery`.
fn prettify_unit(unit: &str) -> String {
    let base = unit
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(unit);

    base.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
