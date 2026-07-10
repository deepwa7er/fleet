//! Builds the HTTP client: routing through the intercepting proxy and deciding
//! how to trust the proxy's on-the-fly TLS certificates.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::redirect::Policy;

use crate::template::FilledRequest;

/// How to trust the proxy's TLS interception. A proxy like Burp mints a leaf
/// certificate per host signed by its own CA, which the system store does not
/// know about, so ordinary verification fails on every HTTPS target.
#[derive(Debug, Clone)]
pub enum TlsTrust {
    /// Normal verification against the system roots. Correct when no proxy is
    /// used, or the proxy CA is already installed system-wide.
    System,
    /// Add the proxy's exported CA (PEM or DER) to the trust store, keeping
    /// full verification of the forged leaf certificates.
    ProxyCa(Vec<u8>),
    /// Disable certificate verification entirely. Convenient for a proxy you
    /// control; scoped to this client only, never a global setting.
    Insecure,
}

impl TlsTrust {
    /// Resolve the trust mode from the mutually-informing CLI flags, reading
    /// the CA file if one was given.
    pub fn resolve(proxy_ca: Option<&Path>, insecure: bool) -> Result<Self> {
        match (proxy_ca, insecure) {
            (Some(_), true) => {
                bail!("--proxy-ca and --insecure are mutually exclusive; pick one")
            }
            (Some(path), false) => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("reading proxy CA from {}", path.display()))?;
                Ok(TlsTrust::ProxyCa(bytes))
            }
            (None, true) => Ok(TlsTrust::Insecure),
            (None, false) => Ok(TlsTrust::System),
        }
    }
}

/// Assemble the client. Redirects are disabled so the exact response to each
/// prompt is observed rather than a followed 3xx, and no default User-Agent is
/// forced — the template carries the real app's headers verbatim.
pub fn build(
    proxy: Option<&str>,
    trust: &TlsTrust,
    timeout: Duration,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(timeout);

    if let Some(url) = proxy {
        let proxy = reqwest::Proxy::all(url)
            .with_context(|| format!("configuring proxy {url}"))?;
        builder = builder.proxy(proxy);
    }

    builder = match trust {
        TlsTrust::System => builder,
        TlsTrust::Insecure => builder.danger_accept_invalid_certs(true),
        TlsTrust::ProxyCa(bytes) => {
            let cert = load_certificate(bytes)?;
            builder.add_root_certificate(cert)
        }
    };

    builder.build().context("building HTTP client")
}

/// Accept the CA in either encoding; a hand-exported cert is as likely to be
/// PEM as the DER that Burp's default export produces.
fn load_certificate(bytes: &[u8]) -> Result<reqwest::Certificate> {
    if let Ok(cert) = reqwest::Certificate::from_pem(bytes) {
        return Ok(cert);
    }
    reqwest::Certificate::from_der(bytes)
        .context("proxy CA is neither valid PEM nor DER")
}

/// Turn a filled template into a live request against the given client.
pub fn to_request(
    client: &reqwest::Client,
    filled: &FilledRequest,
) -> Result<reqwest::Request> {
    let method = reqwest::Method::from_bytes(filled.method.as_bytes())
        .with_context(|| format!("invalid HTTP method {:?}", filled.method))?;

    let mut req = client.request(method, &filled.url);
    for (name, value) in &filled.headers {
        req = req.header(name, value);
    }
    req = req.body(filled.body.clone());

    req.build().context("building request")
}
