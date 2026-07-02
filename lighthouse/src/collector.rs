//! Background health collector.
//!
//! On an interval, records each monitored service's systemd state and memory,
//! plus an *out-of-loopback* reachability probe: an HTTPS GET of the service's
//! public URL through breakwater, exercising real TLS and host routing. That
//! catches a class of failure both the loopback health-check and the systemd
//! unit state miss — a cert mismatch, a misrouted host, or a dead upstream behind
//! a still-running proxy (exactly the false-"healthy" the domain move exposed).
//!
//! Samples land in the history [`Store`], pruned to a retention window. Like the
//! alert watcher, the probe shells out to `curl` rather than pulling a
//! TLS-enabled HTTP client into the build — and `curl` validates the certificate
//! by default, so a bad cert surfaces as a failed probe.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::Command;

use crate::config::Config;
use crate::store::{Probe, Sample, Store};
use crate::systemd;

/// Floor on the sample interval, so a misconfigured tiny value can't hammer
/// `systemctl` and `curl`.
const MIN_INTERVAL_SECS: u64 = 10;

/// Run the collector loop. The caller only spawns this when history is enabled
/// and the store opened, so it always has somewhere to write.
pub async fn run(config: Config, store: Arc<Store>) {
    let history = &config.history;
    let interval = Duration::from_secs(history.interval_secs.max(MIN_INTERVAL_SECS));
    let retention_secs = history.retention_days.max(1) * 86_400;
    let probe_timeout = history.probe_timeout_secs.max(1);
    println!(
        "history collector sampling every {}s (retention {}d)",
        interval.as_secs(),
        history.retention_days,
    );

    loop {
        let now = now_unix();
        // Prune first, so even a never-restarted process stays bounded.
        if let Err(err) = store.prune(now - retention_secs) {
            eprintln!("history: prune failed: {err:#}");
        }

        let units = monitored_units(&config).await;
        // Reference data (the public URL to probe), reloaded each sweep so a newly
        // routed service starts being probed without a restart. Best-effort.
        let fleet = crate::fleet::load(&config.fleet_path);

        for unit in &units {
            let status = match systemd::status(unit, unit).await {
                Ok(status) => status,
                Err(err) => {
                    eprintln!("history: probing systemd state of {unit} failed: {err:#}");
                    continue;
                }
            };
            let probe = match fleet.get(unit).and_then(|f| f.url.clone()) {
                Some(url) => Some(probe(&url, probe_timeout).await),
                None => None,
            };
            let sample = Sample {
                at: now,
                active_state: status.active_state,
                memory_bytes: status.memory_bytes.map(|m| m as i64),
                probe,
            };
            if let Err(err) = store.insert(unit, &sample) {
                eprintln!("history: recording {unit} failed: {err:#}");
            }
        }

        tokio::time::sleep(interval).await;
    }
}

/// Fetch `url` through breakwater and classify the result. A non-2xx/3xx but
/// sub-500 response (e.g. a service whose `/` is a 404) still counts as reachable:
/// it proves TLS, host routing, and a live upstream. Only a TLS/connection
/// failure or a 5xx is "down".
async fn probe(url: &str, timeout_secs: u64) -> Probe {
    let result = Command::new("curl")
        .args([
            "-sS",
            "-o",
            "/dev/null",
            "-m",
            &timeout_secs.to_string(),
            "-w",
            "%{http_code} %{time_total}",
            url,
        ])
        .output()
        .await;

    match result {
        // curl exited cleanly: it got an HTTP response (the body code may still be
        // an error status, which we classify below).
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut parts = text.split_whitespace();
            let status = parts.next().and_then(|s| s.parse::<u16>().ok()).filter(|&c| c != 0);
            let ms = parts
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .map(|secs| (secs * 1000.0) as u32);
            Probe { ok: status.is_some_and(|c| c < 500), status, ms }
        }
        // Non-zero exit: a TLS error (cert mismatch — curl exit 60), connection
        // refused, or timeout. Unreachable through breakwater.
        Ok(_) => Probe { ok: false, status: None, ms: None },
        Err(err) => {
            eprintln!("history: could not run curl for {url}: {err}");
            Probe { ok: false, status: None, ms: None }
        }
    }
}

/// The monitored set: target members ∪ pinned units, as unit names.
async fn monitored_units(config: &Config) -> Vec<String> {
    let mut units = systemd::discover_units(&config.target).await.unwrap_or_default();
    for pinned in config.pinned_units() {
        if !units.iter().any(|u| u == pinned) {
            units.push(pinned.to_owned());
        }
    }
    units.sort();
    units.dedup();
    units
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
