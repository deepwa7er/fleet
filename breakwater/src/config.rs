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

/// Let's Encrypt's production directory — the default ACME endpoint.
fn default_directory() -> String {
    "https://acme-v02.api.letsencrypt.org/directory".to_string()
}

/// Renew once the certificate has fewer than this many days of validity left.
fn default_renew_before_days() -> i64 {
    30
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Port for the TLS listener — the tailnet-facing front door. Every service
    /// is reached here as `https://<name>.internal.deepwa7er.com`. breakwater
    /// resolves the host's Tailscale IPv4 at startup and binds this port on it,
    /// so it is reachable only from the tailnet and the config carries no
    /// host-specific address (see [`crate::tailscale`]).
    pub https_port: u16,

    /// Optional plain-HTTP listener (on the same Tailscale IP) that 308-redirects
    /// every request to its `https://` equivalent. Omit to not listen on `:80`.
    #[serde(default)]
    pub http_redirect_port: Option<u16>,

    /// Optional loopback health endpoint for tugboat's deploy health check.
    /// Kept off the tailnet listener so it is never reachable by clients.
    #[serde(default)]
    pub health_addr: Option<SocketAddr>,

    /// Static-certificate mode: serve a certificate and key from disk. Mutually
    /// exclusive with `[acme]`; exactly one of the two must be set.
    #[serde(default)]
    pub tls: Option<TlsConfig>,

    /// Automatic-certificate mode: obtain and renew a certificate via ACME
    /// DNS-01 (Cloudflare). Mutually exclusive with `[tls]`.
    #[serde(default)]
    pub acme: Option<AcmeConfig>,

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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcmeConfig {
    /// Certificate subject names, e.g. `["*.internal.deepwa7er.com"]`.
    pub domains: Vec<String>,
    /// ACME account contact, as a full URI, e.g. `mailto:you@example.com`.
    pub contact: String,
    /// ACME directory URL. Defaults to Let's Encrypt production; point at the
    /// staging directory while testing to avoid rate limits.
    #[serde(default = "default_directory")]
    pub directory: String,
    /// The Cloudflare DNS zone that hosts the challenge records, e.g.
    /// `deepwa7er.com`. Stated explicitly rather than guessed from the domain
    /// (which would need a public-suffix list to do correctly).
    pub cloudflare_zone: String,
    /// File containing the Cloudflare API token (DNS:Edit + Zone:Read). Kept out
    /// of config and git; install it on the host at mode 600.
    pub cloudflare_token_file: PathBuf,
    /// Directory where the ACME account key and issued cert/key are cached, so a
    /// restart reuses the existing certificate instead of re-issuing.
    pub cache_dir: PathBuf,
    #[serde(default = "default_renew_before_days")]
    pub renew_before_days: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// The public hostname clients use, e.g. `lighthouse.internal.deepwa7er.com`.
    pub host: String,
    /// Reverse-proxy target: the local `host:port` to forward matching requests
    /// to, e.g. `127.0.0.1:8080`. Exactly one of `upstream` or `serve_dir` is set.
    #[serde(default)]
    pub upstream: Option<String>,
    /// Static-site target: an absolute directory served straight from disk, with
    /// any path that doesn't resolve to a file falling back to its `index.html`
    /// (so a single-page app's client-side routes load the app shell). Exactly
    /// one of `upstream` or `serve_dir` is set.
    #[serde(default)]
    pub serve_dir: Option<PathBuf>,
}

/// What a matched route does with a request, resolved from a [`Route`] once the
/// config has validated that exactly one target is set.
#[derive(Debug, Clone)]
pub enum RouteTarget {
    /// Reverse-proxy to a local `host:port` upstream.
    Proxy(String),
    /// Serve a static directory (SPA fallback to its `index.html`).
    Static(PathBuf),
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
        match (&self.tls, &self.acme) {
            (Some(_), Some(_)) => bail!("set either [tls] or [acme], not both"),
            (None, None) => bail!("set exactly one of [tls] (static cert) or [acme] (auto cert)"),
            (Some(_), None) => {}
            (None, Some(acme)) => {
                if acme.domains.is_empty() {
                    bail!("[acme] needs at least one domain");
                }
            }
        }
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
            // A route forwards to an upstream or serves a directory — never both,
            // never neither — so the request-time target is unambiguous.
            match (&route.upstream, &route.serve_dir) {
                (Some(_), Some(_)) => {
                    bail!("route {host:?} sets both `upstream` and `serve_dir`; set exactly one")
                }
                (None, None) => {
                    bail!("route {host:?} sets neither `upstream` nor `serve_dir`; set exactly one")
                }
                (Some(upstream), None) => {
                    // An upstream must be a real `host:port` authority — a missing
                    // port is the most likely mistake, so name it specifically.
                    let authority: Authority = upstream.parse().with_context(|| {
                        format!("route {host:?} has an invalid upstream {upstream:?}")
                    })?;
                    if authority.port_u16().is_none() {
                        bail!("route {host:?} upstream {upstream:?} is missing a port");
                    }
                }
                (None, Some(dir)) => {
                    // Resolved against the host's filesystem at request time, so it
                    // must be absolute; it need not exist yet (the site may deploy
                    // after breakwater starts).
                    if !dir.is_absolute() {
                        bail!("route {host:?} serve_dir {dir:?} must be an absolute path");
                    }
                }
            }
        }
        Ok(())
    }

    /// Build the hostname → target lookup used at request time. Hosts are
    /// lowercased so matching is case-insensitive. [`validate`] has already
    /// guaranteed every route resolves to exactly one target.
    pub fn routing_table(&self) -> HashMap<String, RouteTarget> {
        self.routes
            .iter()
            .map(|r| {
                let target = match (&r.upstream, &r.serve_dir) {
                    (Some(upstream), None) => RouteTarget::Proxy(upstream.clone()),
                    (None, Some(dir)) => RouteTarget::Static(dir.clone()),
                    _ => unreachable!("validate() guarantees exactly one target per route"),
                };
                (r.host.trim().to_ascii_lowercase(), target)
            })
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
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_upstream_without_port() {
        let toml = r#"
            https_port = 443
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
            https_port = 443
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
    fn rejects_both_tls_and_acme() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [acme]
            domains = ["*.internal.deepwa7er.com"]
            contact = "mailto:a@b.com"
            cloudflare_zone = "deepwa7er.com"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1:8080"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_neither_tls_nor_acme() {
        let toml = r#"
            https_port = 443
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1:8080"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn acme_directory_defaults_to_production() {
        let toml = r#"
            https_port = 443
            [acme]
            domains = ["*.internal.deepwa7er.com"]
            contact = "mailto:a@b.com"
            cloudflare_zone = "deepwa7er.com"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        let acme = config.acme.unwrap();
        assert!(acme.directory.contains("acme-v02.api.letsencrypt.org"));
        assert_eq!(acme.renew_before_days, 30);
    }

    #[test]
    fn routing_table_lowercases_hosts() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "Mixed.Example.COM"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        let table = config.routing_table();
        match table.get("mixed.example.com") {
            Some(RouteTarget::Proxy(upstream)) => assert_eq!(upstream, "127.0.0.1:8080"),
            other => panic!("expected a proxy target, got {other:?}"),
        }
    }

    #[test]
    fn accepts_static_route_and_builds_target() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "docs.internal.deepwa7er.com"
            serve_dir = "/opt/pilot/web"
        "#;
        let config = Config::from_toml(toml).unwrap();
        let table = config.routing_table();
        match table.get("docs.internal.deepwa7er.com") {
            Some(RouteTarget::Static(dir)) => assert_eq!(dir, Path::new("/opt/pilot/web")),
            other => panic!("expected a static target, got {other:?}"),
        }
    }

    #[test]
    fn rejects_route_with_both_targets() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "a.example.com"
            upstream = "127.0.0.1:8080"
            serve_dir = "/opt/site"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_route_with_neither_target() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "a.example.com"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_relative_serve_dir() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            host = "a.example.com"
            serve_dir = "opt/site"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }
}
