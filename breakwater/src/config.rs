//! Configuration: where breakwater listens, which TLS cert it serves, and the
//! hostname → local-service routing table.
//!
//! The config is loaded once at startup so a malformed file fails fast (and
//! loudly, under the journal) rather than at the first request.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use http::uri::Authority;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// TLS listener — the tailnet-facing front door. Every service is reached
    /// here as `https://<name>.internal.deepwa7er.com`. Binds the Tailscale IP,
    /// so it is reachable only from the tailnet.
    pub https_addr: SocketAddr,

    /// Optional plain-HTTP listener that 308-redirects every request to its
    /// `https://` equivalent. Omit to not listen on `:80` at all.
    #[serde(default)]
    pub http_redirect_addr: Option<SocketAddr>,

    /// Optional loopback health endpoint for tugboat's deploy health check.
    /// Kept off the tailnet listener so it is never reachable by clients.
    #[serde(default)]
    pub health_addr: Option<SocketAddr>,

    pub tls: TlsConfig,

    #[serde(default)]
    pub routes: Vec<Route>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM file: the leaf certificate followed by any intermediates (fullchain).
    pub cert: PathBuf,
    /// PEM file: the matching private key.
    pub key: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// The public hostname clients use, e.g. `lighthouse.internal.deepwa7er.com`.
    pub host: String,
    /// The local `host:port` to forward matching requests to, e.g. `127.0.0.1:8080`.
    pub upstream: String,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Config::from_toml(&text)
            .with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Config> {
        let config: Config = toml::from_str(text).context("failed to parse config")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.routes.is_empty() {
            bail!("no [[routes]] defined — breakwater would forward nothing");
        }
        let mut seen = HashMap::new();
        for route in &self.routes {
            let host = route.host.trim();
            if host.is_empty() {
                bail!("a route has an empty `host`");
            }
            // Hostnames are case-insensitive; collisions that differ only in case
            // would route ambiguously, so reject them up front.
            let key = host.to_ascii_lowercase();
            if let Some(prev) = seen.insert(key, host.to_string()) {
                bail!("duplicate route host {host:?} (also {prev:?})");
            }
            // An upstream must be a real `host:port` authority — a missing port
            // is the most likely mistake, so name it specifically.
            let authority: Authority = route
                .upstream
                .parse()
                .with_context(|| format!("route {host:?} has an invalid upstream {:?}", route.upstream))?;
            if authority.port_u16().is_none() {
                bail!("route {host:?} upstream {:?} is missing a port", route.upstream);
            }
        }
        Ok(())
    }

    /// Build the hostname → upstream lookup used at request time. Hosts are
    /// lowercased so matching is case-insensitive.
    pub fn routing_table(&self) -> HashMap<String, String> {
        self.routes
            .iter()
            .map(|r| (r.host.trim().to_ascii_lowercase(), r.upstream.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../breakwater.toml");

    #[test]
    fn bundled_sample_config_is_valid() {
        Config::from_toml(SAMPLE).expect("bundled breakwater.toml must parse and validate");
    }

    #[test]
    fn rejects_empty_routes() {
        let toml = r#"
            https_addr = "100.98.184.58:443"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_upstream_without_port() {
        let toml = r#"
            https_addr = "100.98.184.58:443"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_duplicate_hosts_differing_only_in_case() {
        let toml = r#"
            https_addr = "100.98.184.58:443"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1:8080"
            [[routes]]
            host = "A.EXAMPLE.COM"
            upstream = "127.0.0.1:8081"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn routing_table_lowercases_hosts() {
        let toml = r#"
            https_addr = "100.98.184.58:443"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "Mixed.Example.COM"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        let table = config.routing_table();
        assert_eq!(table.get("mixed.example.com").map(String::as_str), Some("127.0.0.1:8080"));
    }
}
