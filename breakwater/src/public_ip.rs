//! Discover the host's public IPv4 for the public breakwater listener.
//!
//! The public listener (`live.deepwa7er.com`) must bind a specific public IP
//! rather than `0.0.0.0:443`, because `0.0.0.0:443` would conflict with the
//! tailnet listener bound to `tailnet_ip:443` (Linux refuses the second bind).
//! So we discover the public IP at startup — same pattern as `crate::tailscale`.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::{bail, Context};

fn is_cgnat(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

fn is_private(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    // 10.0.0.0/8
    if oct[0] == 10 {
        return true;
    }
    // 172.16.0.0/12
    if oct[0] == 172 && (16..=31).contains(&oct[1]) {
        return true;
    }
    // 192.168.0.0/16
    if oct[0] == 192 && oct[1] == 168 {
        return true;
    }
    false
}

fn is_link_local(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    oct[0] == 169 && oct[1] == 254
}

fn is_public_candidate(ip: Ipv4Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !is_cgnat(ip)
        && !is_private(ip)
        && !is_link_local(ip)
        && !ip.is_multicast()
        && !ip.is_broadcast()
}

async fn find_once() -> anyhow::Result<Option<Ipv4Addr>> {
    // Env override for dev machines and tests: `BREAKWATER_PUBLIC_IP=127.0.0.1` etc.
    if let Ok(val) = std::env::var("BREAKWATER_PUBLIC_IP") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let ip: Ipv4Addr = trimmed
                .parse()
                .with_context(|| format!("invalid BREAKWATER_PUBLIC_IP {trimmed:?}"))?;
            return Ok(Some(ip));
        }
    }
    // Try DigitalOcean metadata service first — it knows the droplet's public IP
    // even when the interface enumeration is ambiguous in some virtualization modes.
    // Best-effort: if it fails we fall through to interface scan.
    if let Some(ip) = try_do_metadata().await {
        return Ok(Some(ip));
    }
    let interfaces = if_addrs::get_if_addrs().context("failed to enumerate network interfaces")?;
    Ok(interfaces
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .find(|&v4| is_public_candidate(v4)))
}

async fn try_do_metadata() -> Option<Ipv4Addr> {
    // DigitalOcean metadata: http://169.254.169.254/metadata/v1/interfaces/public/0/ipv4/address
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_millis(400))
        .build()
        .ok()?
        .get("http://169.254.169.254/metadata/v1/interfaces/public/0/ipv4/address")
        .timeout(Duration::from_millis(400))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    text.trim().parse::<Ipv4Addr>().ok()
}

/// Resolve the host's public IPv4, retrying until it appears or `timeout` elapses.
pub async fn resolve(timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    const POLL: Duration = Duration::from_millis(500);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(ip) = find_once().await? {
            return Ok(ip);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "no public IPv4 on any interface after {}s — set BREAKWATER_PUBLIC_IP or ensure the host has a public address",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_candidate_filters() {
        // Public
        assert!(is_public_candidate(Ipv4Addr::new(147, 182, 250, 13)));
        assert!(is_public_candidate(Ipv4Addr::new(8, 8, 8, 8)));
        // Private / CGNAT / loopback / link-local are not public
        assert!(!is_public_candidate(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!is_public_candidate(Ipv4Addr::new(192, 168, 1, 10)));
        assert!(!is_public_candidate(Ipv4Addr::new(172, 20, 5, 3)));
        assert!(!is_public_candidate(Ipv4Addr::new(100, 98, 184, 58))); // CGNAT
        assert!(!is_public_candidate(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_public_candidate(Ipv4Addr::new(169, 254, 10, 20)));
    }

    #[tokio::test]
    async fn env_override_is_honored() {
        // Safety: set env var, test, then remove. Tests run in parallel so use a
        // unique value and restore.
        let prev = std::env::var("BREAKWATER_PUBLIC_IP").ok();
        unsafe {
            std::env::set_var("BREAKWATER_PUBLIC_IP", "203.0.113.7");
        }
        let ip = find_once().await.unwrap().unwrap();
        assert_eq!(ip, Ipv4Addr::new(203, 0, 113, 7));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("BREAKWATER_PUBLIC_IP", v),
                None => std::env::remove_var("BREAKWATER_PUBLIC_IP"),
            }
        }
    }
}
