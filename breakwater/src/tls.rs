//! TLS setup: load the wildcard certificate and key from disk and build a
//! rustls server config.
//!
//! Certificates are loaded once at startup. Automatic renewal (ACME DNS-01 via
//! Cloudflare) and hot-reload are a later step; this module is the single place
//! that turns PEM files into a serving config, so that change lands here.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::TlsConfig;

/// Build a TLS acceptor from the configured certificate and key.
pub fn acceptor(config: &TlsConfig) -> anyhow::Result<TlsAcceptor> {
    let certs = load_certs(&config.cert)?;
    let key = load_key(&config.key)?;

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("certificate and key do not form a valid TLS config")?;
    // breakwater only speaks HTTP/1.1 (backends are plain HTTP/1.1, and upgrade
    // tunneling is an HTTP/1.1 mechanism), so advertise exactly that via ALPN.
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

fn load_certs(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let pem = std::fs::read(path)
        .with_context(|| format!("failed to read certificate {}", path.display()))?;
    let certs = rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse certificates in {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificates found in {}", path.display());
    }
    Ok(certs)
}

fn load_key(path: &Path) -> anyhow::Result<PrivateKeyDer<'static>> {
    let pem = std::fs::read(path)
        .with_context(|| format!("failed to read private key {}", path.display()))?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        .with_context(|| format!("failed to parse private key in {}", path.display()))?
        .with_context(|| format!("no private key found in {}", path.display()))
}
