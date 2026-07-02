//! ACME certificate issuance over the DNS-01 challenge, solved via Cloudflare.
//!
//! Flow: create an order for the configured domains, publish each
//! `_acme-challenge` TXT record, wait for it to actually be visible in DNS,
//! tell the ACME server the challenges are ready, then finalize and download
//! the certificate. Challenge records are always cleaned up afterwards, whether
//! issuance succeeded or failed.

use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use anyhow::{Context, bail};
use hickory_resolver::TokioAsyncResolver;
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};

use crate::cert::{CertMaterial, account_cache_path};
use crate::cloudflare::Cloudflare;
use crate::config::AcmeConfig;

/// Obtain a certificate for the configured domains. Reuses a cached ACME
/// account if present, creating one otherwise.
pub async fn issue(config: &AcmeConfig, token: &str) -> anyhow::Result<CertMaterial> {
    let cloudflare = Cloudflare::new(token.to_string());
    let zone = cloudflare
        .zone(&config.cloudflare_zone)
        .await
        .with_context(|| format!("looking up Cloudflare zone {}", config.cloudflare_zone))?;
    let account = load_or_create_account(config).await?;

    // Track every record we create so we can always remove them, even on error.
    let mut created_records = Vec::new();
    let result = obtain(config, &account, &cloudflare, &zone, &mut created_records).await;
    for record_id in &created_records {
        if let Err(err) = cloudflare.delete_record(&zone.id, record_id).await {
            eprintln!("breakwater: failed to clean up challenge record {record_id}: {err:#}");
        }
    }
    result
}

async fn obtain(
    config: &AcmeConfig,
    account: &Account,
    cloudflare: &Cloudflare,
    zone: &crate::cloudflare::Zone,
    created_records: &mut Vec<String>,
) -> anyhow::Result<CertMaterial> {
    let identifiers: Vec<Identifier> = config
        .domains
        .iter()
        .map(|domain| Identifier::Dns(domain.clone()))
        .collect();
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .context("creating ACME order")?;

    // For each pending authorization: publish its dns-01 TXT record, wait for
    // it to actually be visible in DNS, then mark the challenge ready. Doing all
    // of this in a single authorizations pass keeps the order's challenge state
    // consistent with the server (a second pass over cached state left the order
    // pending at finalization). The Cloudflare and DNS calls don't touch `order`,
    // so the borrow of the challenge handle is fine across them.
    let resolver = authoritative_resolver(&zone.name_servers).await?;
    {
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut handle = result.context("reading authorization")?;
            if handle.status == AuthorizationStatus::Valid {
                continue; // already authorized from a previous run
            }
            let mut challenge = handle
                .challenge(ChallengeType::Dns01)
                .context("authorization offers no dns-01 challenge")?;
            let domain = match challenge.identifier().identifier {
                Identifier::Dns(dns) => dns.clone(),
                other => bail!("unexpected ACME identifier type: {other:?}"),
            };
            let value = challenge.key_authorization().dns_value();
            let record_name = format!("_acme-challenge.{domain}");

            let record_id = cloudflare
                .create_txt(&zone.id, &record_name, &value)
                .await
                .with_context(|| format!("creating TXT record {record_name}"))?;
            created_records.push(record_id);

            // A not-yet-propagated record fails the challenge permanently, so
            // confirm the record is live before telling the CA to validate.
            await_record_visible(&resolver, &record_name, &value).await?;
            challenge.set_ready().await.context("marking challenge ready")?;
        }
    }

    let status = order
        .poll_ready(&RetryPolicy::default())
        .await
        .context("waiting for the ACME order to be ready")?;
    if status != OrderStatus::Ready {
        bail!("ACME order did not become ready (status {status:?})");
    }

    // finalize() generates the certificate key + CSR (via rcgen) and returns the
    // private key as PEM; poll_certificate() returns the issued chain.
    let key_pem = order.finalize().await.context("finalizing order")?;
    let chain_pem = order
        .poll_certificate(&RetryPolicy::default())
        .await
        .context("downloading certificate")?;

    Ok(CertMaterial { chain_pem, key_pem })
}

/// Poll DNS until a challenge record is visible, or give up after ~2.5 minutes.
/// The resolver queries the zone's *authoritative* nameservers directly with
/// caching disabled, so neither a public resolver's negative cache nor an
/// in-process cached first-miss can hide a record that is already live.
async fn await_record_visible(
    resolver: &TokioAsyncResolver,
    record_name: &str,
    record_value: &str,
) -> anyhow::Result<()> {
    let fqdn = format!("{record_name}.");
    for _ in 0..30 {
        if let Ok(lookup) = resolver.txt_lookup(fqdn.clone()).await {
            let found = lookup.iter().any(|txt| {
                txt.txt_data().iter().any(|data| data.as_ref() == record_value.as_bytes())
            });
            if found {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("TXT record {record_name} did not propagate in time");
}

/// Build a resolver that queries the given authoritative nameservers directly,
/// with caching disabled so every poll reflects current zone state.
async fn authoritative_resolver(name_servers: &[String]) -> anyhow::Result<TokioAsyncResolver> {
    let mut ips: Vec<IpAddr> = Vec::new();
    for ns in name_servers {
        let addrs = tokio::net::lookup_host((ns.as_str(), 53))
            .await
            .with_context(|| format!("resolving nameserver {ns}"))?;
        ips.extend(addrs.map(|addr| addr.ip()));
    }
    if ips.is_empty() {
        bail!("no authoritative nameserver addresses resolved");
    }

    let group = NameServerConfigGroup::from_ips_clear(&ips, 53, true);
    let config = ResolverConfig::from_parts(None, Vec::new(), group);
    let mut opts = ResolverOpts::default();
    opts.cache_size = 0;
    Ok(TokioAsyncResolver::tokio(config, opts))
}

/// Load the cached ACME account, or register a new one and cache its credentials.
async fn load_or_create_account(config: &AcmeConfig) -> anyhow::Result<Account> {
    let path = account_cache_path(&config.cache_dir);
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading cached ACME account {}", path.display()))?;
        let credentials: AccountCredentials =
            serde_json::from_str(&data).context("parsing cached ACME account")?;
        return Account::builder()?
            .from_credentials(credentials)
            .await
            .context("restoring ACME account");
    }

    let (account, credentials) = Account::builder()?
        .create(
            &NewAccount {
                contact: &[config.contact.as_str()],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            config.directory.clone(),
            None,
        )
        .await
        .context("creating ACME account")?;

    std::fs::create_dir_all(&config.cache_dir)
        .with_context(|| format!("creating cache dir {}", config.cache_dir.display()))?;
    let data = serde_json::to_string(&credentials).context("serializing ACME account")?;
    std::fs::write(&path, &data).with_context(|| format!("writing {}", path.display()))?;
    // The account credentials embed the account key — keep them private.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("securing {}", path.display()))?;

    Ok(account)
}
