//! Runtime configuration for `spyglass serve`, loaded from a TOML file.
//!
//! `bind` is loopback on the VPS; breakwater is the tailnet front door (the
//! tailnet is the security boundary — no auth here, like the rest of the fleet).
//! `[[sources]]` lists the upstream search services to federate over; each is
//! reached at its real tailnet/loopback address over plain HTTP.

use std::net::IpAddr;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address to bind (loopback; breakwater fronts it).
    pub bind: IpAddr,
    /// TCP port to listen on.
    pub port: u16,
    /// GitHub org the fleet's repos live under, for building code deep-links.
    #[serde(default = "default_github_org")]
    pub github_org: String,
    /// The upstream search services to query, in display order.
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    /// Stable id (also the upstream service's fleet name).
    pub name: String,
    /// What it searches — selects the response adapter.
    pub kind: SourceKind,
    /// Base URL, e.g. `http://127.0.0.1:8092` or `http://100.74.202.93:7879`.
    pub url: String,
    /// Display label; defaults to a title-cased `name`.
    #[serde(default)]
    pub label: Option<String>,
}

impl SourceConfig {
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| title_case(&self.name))
    }
}

/// The kind of search a source exposes, which picks the response adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// A code search (the `source` service): ripgrep over the fleet's repos.
    Code,
    /// A notes search (the `lagoon` service): FTS over thoughts.
    Notes,
}

fn default_github_org() -> String {
    "deepwa7er".to_owned()
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading config {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("parsing config {}: {e}", path.display()))
    }
}
