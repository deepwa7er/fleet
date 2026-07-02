//! Discover the host's Tailscale IPv4 address at startup.
//!
//! breakwater binds its tailnet-facing listeners to the host's Tailscale IP so
//! it is reachable only from the tailnet. Rather than baking that IP into the
//! config — which then can't ship verbatim and drifts per host — we discover it
//! at runtime: Tailscale assigns every node an address in the carrier-grade NAT
//! range `100.64.0.0/10`, so we enumerate local interfaces and pick that one.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::{Context, bail};

/// Tailscale hands each node an IPv4 in the CGNAT block `100.64.0.0/10`
/// (`100.64.0.0` – `100.127.255.255`). On the fleet hosts that block is only
/// ever the Tailscale interface, so membership is a reliable identifier.
fn is_tailscale_cgnat(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// One enumeration pass: the first interface IPv4 in the Tailscale CGNAT range,
/// or `None` if the tailnet address hasn't been assigned yet.
fn find_once() -> anyhow::Result<Option<Ipv4Addr>> {
    let interfaces = if_addrs::get_if_addrs().context("failed to enumerate network interfaces")?;
    Ok(interfaces
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .find(|&v4| is_tailscale_cgnat(v4)))
}

/// Resolve the host's Tailscale IPv4, retrying until it appears or `timeout`
/// elapses. The systemd unit orders breakwater after `tailscaled`, but that only
/// guarantees the daemon has *started* — the tailnet address can take a moment
/// to be assigned, so we poll rather than fail on that startup race.
pub async fn resolve(timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    const POLL: Duration = Duration::from_millis(500);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(ip) = find_once()? {
            return Ok(ip);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "no Tailscale IPv4 (100.64.0.0/10) on any interface after {}s — \
                 is tailscaled running and the node logged in?",
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
    fn cgnat_range_membership() {
        // Inside 100.64.0.0/10 — including this fleet's actual node address.
        assert!(is_tailscale_cgnat(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(is_tailscale_cgnat(Ipv4Addr::new(100, 98, 184, 58)));
        assert!(is_tailscale_cgnat(Ipv4Addr::new(100, 127, 255, 255)));
        // Just outside the /10 boundaries.
        assert!(!is_tailscale_cgnat(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_tailscale_cgnat(Ipv4Addr::new(100, 128, 0, 0)));
        // Unrelated ranges: loopback, RFC1918, and this box's public IP.
        assert!(!is_tailscale_cgnat(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_tailscale_cgnat(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!is_tailscale_cgnat(Ipv4Addr::new(147, 182, 250, 13)));
    }
}
