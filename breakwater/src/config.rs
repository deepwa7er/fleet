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
    /// is reached here as `https://<name>.intern.deepwa7er.net`. breakwater
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

    /// The subdomain space every routed service lives under, e.g.
    /// `intern.deepwa7er.net`. Each route's `label` is prefixed to this to form
    /// the hostname clients use (`<label>.<base_domain>`), and — unless `[acme]
    /// domains` is set explicitly — the ACME certificate is a wildcard over it
    /// (`*.<base_domain>`). This field and `[acme] cloudflare_zone` are the only
    /// two places the fleet's domain is written; change them to move the fleet.
    pub base_domain: String,

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
    /// Certificate subject names. Defaults to a single wildcard over the
    /// top-level `base_domain` (`*.<base_domain>`), which covers every routed
    /// host; set explicitly only to request additional or different names.
    #[serde(default)]
    pub domains: Vec<String>,
    /// ACME account contact, as a full URI, e.g. `mailto:you@example.com`.
    pub contact: String,
    /// ACME directory URL. Defaults to Let's Encrypt production; point at the
    /// staging directory while testing to avoid rate limits.
    #[serde(default = "default_directory")]
    pub directory: String,
    /// The Cloudflare DNS zone that hosts the challenge records, e.g.
    /// `deepwa7er.net`. Stated explicitly rather than guessed from the domain
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
    /// The service's subdomain label, e.g. `lighthouse`. The full hostname
    /// clients use is `<label>.<base_domain>` (see [`Config::base_domain`]).
    pub label: String,
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

impl Route {
    /// The full hostname this route matches: `<label>.<base_domain>`.
    fn host(&self, base_domain: &str) -> String {
        format!("{}.{}", self.label.trim(), base_domain.trim())
    }
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
        let mut config: Config = toml::from_str(text).context("failed to parse config")?;
        config.resolve();
        config.validate()?;
        Ok(config)
    }

    /// Fill in values derived from `base_domain` so the domain is written once.
    /// Currently: an `[acme]` block with no explicit `domains` gets a wildcard
    /// over the base domain, which is exactly the set of hosts breakwater routes.
    fn resolve(&mut self) {
        let base = self.base_domain.trim().to_string();
        if let Some(acme) = self.acme.as_mut()
            && acme.domains.is_empty()
            && !base.is_empty()
        {
            acme.domains = vec![format!("*.{base}")];
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.base_domain.trim().is_empty() {
            bail!("`base_domain` must be set (e.g. \"internal.example.com\")");
        }
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
            if route.label.trim().is_empty() {
                bail!("a route has an empty `label`");
            }
            let host = route.host(&self.base_domain);
            let host = host.as_str();
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
    /// lowercased so matching is case-insensitive. Config validation has already
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
                (r.host(&self.base_domain).to_ascii_lowercase(), target)
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
            base_domain = "example.com"
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
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_duplicate_hosts_differing_only_in_case() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
            [[routes]]
            label = "A"
            upstream = "127.0.0.1:8081"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_both_tls_and_acme() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [acme]
            domains = ["*.intern.deepwa7er.net"]
            contact = "mailto:a@b.com"
            cloudflare_zone = "deepwa7er.net"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_neither_tls_nor_acme() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn acme_directory_defaults_to_production() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [acme]
            domains = ["*.intern.deepwa7er.net"]
            contact = "mailto:a@b.com"
            cloudflare_zone = "deepwa7er.net"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            label = "a"
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
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "Mixed"
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
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "docs"
            serve_dir = "/opt/pilot/web"
        "#;
        let config = Config::from_toml(toml).unwrap();
        let table = config.routing_table();
        match table.get("docs.example.com") {
            Some(RouteTarget::Static(dir)) => assert_eq!(dir, Path::new("/opt/pilot/web")),
            other => panic!("expected a static target, got {other:?}"),
        }
    }

    #[test]
    fn rejects_route_with_both_targets() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
            serve_dir = "/opt/site"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_route_with_neither_target() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn composes_host_from_label_and_base_domain() {
        let toml = r#"
            https_port = 443
            base_domain = "internal.example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "lighthouse"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        assert!(config.routing_table().contains_key("lighthouse.internal.example.com"));
    }

    #[test]
    fn acme_domains_default_to_wildcard_over_base_domain() {
        let toml = r#"
            https_port = 443
            base_domain = "internal.example.com"
            [acme]
            contact = "mailto:a@b.com"
            cloudflare_zone = "example.com"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(config.acme.unwrap().domains, vec!["*.internal.example.com"]);
    }

    #[test]
    fn explicit_acme_domains_override_the_derived_wildcard() {
        let toml = r#"
            https_port = 443
            base_domain = "internal.example.com"
            [acme]
            domains = ["one.example.com", "two.example.com"]
            contact = "mailto:a@b.com"
            cloudflare_zone = "example.com"
            cloudflare_token_file = "/etc/breakwater/cloudflare-token"
            cache_dir = "/etc/breakwater/acme"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
        "#;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(
            config.acme.unwrap().domains,
            vec!["one.example.com", "two.example.com"]
        );
    }

    #[test]
    fn rejects_missing_base_domain() {
        let toml = r#"
            https_port = 443
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
            upstream = "127.0.0.1:8080"
        "#;
        // `base_domain` is required: deny_unknown_fields means a missing required
        // field is a parse error.
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn rejects_relative_serve_dir() {
        let toml = r#"
            https_port = 443
            base_domain = "example.com"
            [tls]
            cert = "/x/cert.pem"
            key = "/x/key.pem"
            [[routes]]
            label = "a"
            serve_dir = "opt/site"
        "#;
        assert!(Config::from_toml(toml).is_err());
    }
}
