use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
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
    /// systemd target whose members are auto-discovered and monitored. A service
    /// joins the dashboard by becoming a dependency of this target (see README).
    #[serde(default = "default_target")]
    pub target: String,
    /// Optional override layer: rename a discovered service's label, or pin an
    /// extra unit that isn't a member of the target. Empty means pure discovery.
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Optional Docker support. When present, the listed containers are
    /// monitored alongside systemd services, reached through a socket-proxy
    /// rather than the raw Docker socket. Absent means systemd-only.
    #[serde(default)]
    pub docker: Option<DockerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    /// systemd unit name, e.g. `notes.service`.
    pub unit: String,
    /// Display label override. If absent, the unit name is prettified.
    pub name: Option<String>,
}

/// Docker integration settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DockerConfig {
    /// Base URL of the docker-socket-proxy, e.g. `http://127.0.0.1:2375`.
    /// Lighthouse never touches the raw Docker socket directly.
    pub proxy_url: String,
    /// Containers to monitor. This is also the control allowlist: only these
    /// containers can be inspected, tailed, or controlled.
    #[serde(default)]
    pub containers: Vec<DockerContainerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerContainerConfig {
    /// Container name as known to Docker, e.g. `navidrome`.
    pub container: String,
    /// Display label override. If absent, the container name is prettified.
    pub name: Option<String>,
}

impl DockerContainerConfig {
    /// The display label: the override if set, else the prettified name.
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| prettify(&self.container))
    }
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
            .unwrap_or_else(|| prettify(unit))
    }
}

/// Turn a unit or container name into a human label: drop any `.suffix` and
/// title-case the words separated by `-`/`_`. `sonar-discovery.service` becomes
/// `Sonar Discovery`; `navidrome` becomes `Navidrome`.
fn prettify(name: &str) -> String {
    let base = name
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(name);

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
