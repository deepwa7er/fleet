//! Certificate material: loading, caching to disk, expiry inspection, and the
//! ACME acquire-then-renew lifecycle.
//!
//! In ACME mode breakwater owns its certificate lifecycle entirely in-process:
//! it obtains a certificate at startup (reusing a cached one if still fresh),
//! serves it through a hot-swappable [`CertResolver`], and a background task
//! renews it ahead of expiry and swaps the new one in with no downtime.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};

use crate::acme;
use crate::config::AcmeConfig;
use crate::tls::CertResolver;

const FULLCHAIN: &str = "fullchain.pem";
const PRIVKEY: &str = "privkey.pem";
const ACCOUNT: &str = "account.json";

/// A PEM certificate chain and its matching private key.
#[derive(Clone)]
pub struct CertMaterial {
    pub chain_pem: String,
    pub key_pem: String,
}

impl CertMaterial {
    pub fn from_files(cert: &Path, key: &Path) -> anyhow::Result<Self> {
        let chain_pem = std::fs::read_to_string(cert)
            .with_context(|| format!("failed to read certificate {}", cert.display()))?;
        let key_pem = std::fs::read_to_string(key)
            .with_context(|| format!("failed to read private key {}", key.display()))?;
        Ok(Self { chain_pem, key_pem })
    }

    /// Load a previously issued certificate from the ACME cache, if present.
    pub fn from_cache(dir: &Path) -> anyhow::Result<Option<Self>> {
        let cert = dir.join(FULLCHAIN);
        let key = dir.join(PRIVKEY);
        if !cert.exists() || !key.exists() {
            return Ok(None);
        }
        Ok(Some(Self::from_files(&cert, &key)?))
    }

    /// Write the certificate and key to the cache directory. The key is written
    /// 0600; the chain (public) 0644. Both are written atomically.
    pub fn persist(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create cache dir {}", dir.display()))?;
        write_atomic(&dir.join(FULLCHAIN), self.chain_pem.as_bytes(), 0o644)?;
        write_atomic(&dir.join(PRIVKEY), self.key_pem.as_bytes(), 0o600)?;
        Ok(())
    }

    /// The certificate's `notAfter`, as a Unix timestamp.
    fn not_after_unix(&self) -> anyhow::Result<i64> {
        let der = rustls_pemfile::certs(&mut self.chain_pem.as_bytes())
            .next()
            .context("no certificate in PEM chain")?
            .context("failed to parse leaf certificate")?;
        let (_, cert) = x509_parser::parse_x509_certificate(der.as_ref())
            .map_err(|e| anyhow!("failed to parse X.509 certificate: {e}"))?;
        Ok(cert.validity().not_after.timestamp())
    }

    /// How long until the certificate should be renewed (0 if already due).
    fn time_until_renewal(&self, renew_before_days: i64) -> anyhow::Result<Duration> {
        let renew_at = self.not_after_unix()? - renew_before_days * 86_400;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Ok(Duration::from_secs((renew_at - now).max(0) as u64))
    }

    fn is_fresh(&self, renew_before_days: i64) -> bool {
        // A cached cert is reusable only if it parses and isn't due for renewal.
        matches!(self.time_until_renewal(renew_before_days), Ok(d) if !d.is_zero())
    }
}

/// Account credentials cache path, alongside the issued certificate.
pub fn account_cache_path(dir: &Path) -> std::path::PathBuf {
    dir.join(ACCOUNT)
}

/// Bring up ACME-managed TLS: obtain a certificate (cached or freshly issued),
/// return a resolver serving it, and spawn the background renewal task.
pub async fn start_acme(config: AcmeConfig) -> anyhow::Result<Arc<CertResolver>> {
    let token = read_token(&config)?;

    let material = match CertMaterial::from_cache(&config.cache_dir)? {
        Some(cached) if cached.is_fresh(config.renew_before_days) => {
            println!("breakwater: using cached certificate from {}", config.cache_dir.display());
            cached
        }
        _ => {
            println!("breakwater: obtaining certificate via ACME ({})", config.directory);
            let issued = acme::issue(&config, &token).await.context("ACME issuance failed")?;
            issued.persist(&config.cache_dir)?;
            println!("breakwater: certificate issued and cached");
            issued
        }
    };

    let resolver = Arc::new(CertResolver::new(&material)?);
    tokio::spawn(renewal_loop(config, token, resolver.clone(), material));
    Ok(resolver)
}

/// Sleep until each certificate is near expiry, then renew and hot-swap it.
/// Issuance failures are retried with a bounded backoff; the existing
/// certificate keeps serving in the meantime.
async fn renewal_loop(
    config: AcmeConfig,
    token: String,
    resolver: Arc<CertResolver>,
    mut current: CertMaterial,
) {
    const RETRY_DELAY: Duration = Duration::from_secs(3600);

    loop {
        let wait = current
            .time_until_renewal(config.renew_before_days)
            .unwrap_or(Duration::ZERO);
        if !wait.is_zero() {
            println!("breakwater: next certificate renewal in {} day(s)", wait.as_secs() / 86_400);
        }
        tokio::time::sleep(wait).await;

        match acme::issue(&config, &token).await {
            Ok(renewed) => {
                if let Err(err) = renewed.persist(&config.cache_dir) {
                    eprintln!("breakwater: failed to cache renewed certificate: {err:#}");
                }
                match resolver.update(&renewed) {
                    Ok(()) => {
                        println!("breakwater: certificate renewed and installed");
                        current = renewed;
                    }
                    Err(err) => {
                        eprintln!("breakwater: renewed certificate could not be installed: {err:#}");
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
            Err(err) => {
                eprintln!("breakwater: certificate renewal failed, retrying in 1h: {err:#}");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

fn read_token(config: &AcmeConfig) -> anyhow::Result<String> {
    let token = std::fs::read_to_string(&config.cloudflare_token_file).with_context(|| {
        format!(
            "failed to read Cloudflare token from {}",
            config.cloudflare_token_file.display()
        )
    })?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!(
            "Cloudflare token file {} is empty",
            config.cloudflare_token_file.display()
        );
    }
    Ok(token)
}

/// Write a file atomically *and durably* with the given Unix mode: write a
/// sibling temp file, fsync it, rename it over the destination, then fsync the
/// directory. A crash leaves either the old file or the fully-written new one —
/// never a truncated/zero-length file or a rename the directory entry lost.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = path.parent().context("path has no parent directory")?;
    let tmp = path.with_extension("tmp");
    // Start from a fresh temp: `mode` only takes effect on creation, so reusing a
    // temp left by a crashed earlier write could keep that file's old (possibly
    // looser) permissions on a key. Remove any stale temp, then create exclusively.
    let _ = std::fs::remove_file(&tmp);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("failed to create {}", tmp.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        // Flush contents to stable storage before the rename publishes the file.
        file.sync_all()
            .with_context(|| format!("failed to fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to install {} (in {})", path.display(), dir.display()))?;
    // Flush the rename itself so the directory entry survives a crash.
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("failed to fsync directory {}", dir.display()))?;
    Ok(())
}
