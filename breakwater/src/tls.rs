//! TLS termination with a hot-swappable certificate.
//!
//! The serving certificate lives behind an [`ArcSwap`], so it can be replaced
//! atomically and lock-free while connections are being served — that is how
//! ACME renewal installs a fresh certificate with zero downtime. Static-cert
//! mode uses the same resolver and simply never swaps.

use std::sync::Arc;

use anyhow::{Context, bail};
use arc_swap::ArcSwap;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring::sign::any_supported_type;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;

use crate::cert::CertMaterial;

/// A [`ResolvesServerCert`] whose certificate can be replaced at runtime.
pub struct CertResolver {
    current: ArcSwap<CertifiedKey>,
}

impl CertResolver {
    pub fn new(material: &CertMaterial) -> anyhow::Result<Self> {
        Ok(Self {
            current: ArcSwap::from_pointee(certified_key(material)?),
        })
    }

    /// Atomically replace the served certificate. In-flight handshakes keep the
    /// old one; new handshakes pick up the new one.
    pub fn update(&self, material: &CertMaterial) -> anyhow::Result<()> {
        self.current.store(Arc::new(certified_key(material)?));
        Ok(())
    }
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CertResolver")
    }
}

impl ResolvesServerCert for CertResolver {
    fn resolve(&self, _client_hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        Some(self.current.load_full())
    }
}

/// Build a TLS acceptor that serves whatever certificate `resolver` currently holds.
pub fn acceptor(resolver: Arc<CertResolver>) -> TlsAcceptor {
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    // breakwater only speaks HTTP/1.1 (backends are plain HTTP/1.1 and upgrade
    // tunnelling is an HTTP/1.1 mechanism), so advertise exactly that via ALPN.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsAcceptor::from(Arc::new(config))
}

/// Turn PEM certificate-chain + key material into a rustls [`CertifiedKey`].
fn certified_key(material: &CertMaterial) -> anyhow::Result<CertifiedKey> {
    let certs = parse_certs(&material.chain_pem)?;
    let key = parse_key(&material.key_pem)?;
    let signing_key = any_supported_type(&key).context("unsupported private key type")?;
    Ok(CertifiedKey::new(certs, signing_key))
}

fn parse_certs(pem: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse certificate chain")?;
    if certs.is_empty() {
        bail!("certificate chain contains no certificates");
    }
    Ok(certs)
}

fn parse_key(pem: &str) -> anyhow::Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut pem.as_bytes())
        .context("failed to parse private key")?
        .context("no private key found")
}
