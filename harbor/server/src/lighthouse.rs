use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One systemd service's health, as reported by lighthouse's `/api/services`.
/// Unknown fields (since, memory, pid, …) are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub unit: String,
    pub name: String,
    /// e.g. "active", "inactive", "failed".
    pub active_state: String,
    /// e.g. "running", "dead", "exited".
    pub sub_state: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Fetch live service health from lighthouse at `base_url`.
pub async fn services(http: &reqwest::Client, base_url: &str) -> Result<Vec<ServiceHealth>> {
    let url = format!("{}/api/services", base_url.trim_end_matches('/'));
    let resp = http.get(&url).send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("lighthouse {url} returned {}", resp.status());
    }
    resp.json().await.context("parsing lighthouse services")
}
