//! Background alert watcher.
//!
//! Polls each monitored unit's state on an interval and pushes a notification
//! when a service transitions into trouble — entering `failed`, or restarting
//! rapidly (a crash loop) — and again when it recovers. Notifications are sent
//! by shelling out to `curl` (like the rest of the backend shells out to
//! `systemctl`), so there is no HTTP-client dependency.
//!
//! It alerts on *transitions* only and seeds silently on the first sweep, so a
//! Lighthouse restart doesn't replay alerts for already-bad services, and a
//! crash loop alerts once rather than every interval.

use std::collections::HashMap;
use std::time::Duration;

use tokio::process::Command;

use crate::config::Config;
use crate::systemd;

/// Minimum poll interval, so a misconfigured tiny value can't hammer systemctl.
const MIN_INTERVAL_SECS: u64 = 5;

/// A jump of at least this many restarts within one interval is treated as a
/// crash loop (a single benign restart stays quiet).
const CRASH_RESTART_DELTA: u64 = 2;

struct Snapshot {
    n_restarts: u64,
    /// Whether we've already alerted for the current trouble episode.
    alerting: bool,
}

/// Run the watcher loop. Returns immediately (does nothing) if alerting is not
/// configured.
pub async fn run(config: Config) {
    let Some(alerts) = config.alerts.clone() else {
        return;
    };
    let interval = Duration::from_secs(alerts.interval_secs.max(MIN_INTERVAL_SECS));
    println!("alert watcher polling every {}s -> {}", interval.as_secs(), alerts.notify_url);

    let mut states: HashMap<String, Snapshot> = HashMap::new();
    let mut first_sweep = true;

    loop {
        let units = monitored_units(&config).await;
        // Forget units that are no longer monitored.
        states.retain(|unit, _| units.iter().any(|(u, _)| u == unit));

        for (unit, name) in &units {
            let (active_state, n_restarts) = match systemd::alert_snapshot(unit).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    eprintln!("alert: failed to probe {unit}: {err:#}");
                    continue;
                }
            };

            let decision = decide(states.get(unit), &active_state, n_restarts, first_sweep, name);
            if let Some((tags, message)) = &decision.notify {
                notify(&alerts.notify_url, name, tags, message).await;
            }
            states.insert(unit.clone(), Snapshot { n_restarts, alerting: decision.alerting });
        }

        first_sweep = false;
        tokio::time::sleep(interval).await;
    }
}

/// What the watcher should do for one unit on one sweep.
struct Decision {
    /// `(tags, message)` to send, or `None` to stay quiet.
    notify: Option<(&'static str, String)>,
    /// The alerting flag to carry into the next sweep.
    alerting: bool,
}

/// Decide whether a unit's current state warrants a notification, given its
/// previous snapshot. Pure, so the transition logic is unit-tested.
///
/// Alerts fire on a transition *into* trouble (entering `failed`, or restarts
/// jumping by [`CRASH_RESTART_DELTA`] within one sweep) and once on recovery to
/// a stable `active`. The `alerting` flag debounces a sustained crash loop to a
/// single alert, and `first_sweep` seeds state silently.
fn decide(
    prev: Option<&Snapshot>,
    active_state: &str,
    n_restarts: u64,
    first_sweep: bool,
    name: &str,
) -> Decision {
    let prev_restarts = prev.map_or(n_restarts, |p| p.n_restarts);
    let was_alerting = prev.is_some_and(|p| p.alerting);

    if first_sweep {
        return Decision { notify: None, alerting: false };
    }

    let failed = active_state == "failed";
    let crash_looping = n_restarts.saturating_sub(prev_restarts) >= CRASH_RESTART_DELTA;
    let stable = active_state == "active" && n_restarts == prev_restarts;

    if (failed || crash_looping) && !was_alerting {
        let notify = if failed {
            ("rotating_light", format!("{name} entered the failed state"))
        } else {
            ("arrows_counterclockwise", format!("{name} is restarting repeatedly ({n_restarts} restarts)"))
        };
        Decision { notify: Some(notify), alerting: true }
    } else if was_alerting && stable {
        Decision { notify: Some(("white_check_mark", format!("{name} recovered"))), alerting: false }
    } else {
        Decision { notify: None, alerting: was_alerting }
    }
}

/// The monitored set: target members ∪ pinned units, with display labels.
async fn monitored_units(config: &Config) -> Vec<(String, String)> {
    let mut units = systemd::discover_units(&config.target).await.unwrap_or_default();
    for pinned in config.pinned_units() {
        if !units.iter().any(|u| u == pinned) {
            units.push(pinned.to_owned());
        }
    }
    units.sort();
    units.dedup();
    units
        .into_iter()
        .map(|unit| {
            let name = config.display_name(&unit);
            (unit, name)
        })
        .collect()
}

/// POST a notification via `curl`. ntfy reads the body as the message and the
/// `Title`/`Tags` headers for the heading and emoji. Failures are logged, never
/// fatal — a missed alert must not take down the watcher.
async fn notify(url: &str, title: &str, tags: &str, message: &str) {
    let result = Command::new("curl")
        .args([
            "-fsS",
            "-m",
            "10",
            "-H",
            &format!("Title: {title}"),
            "-H",
            &format!("Tags: {tags}"),
            "-d",
            message,
            url,
        ])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!("alert: notify failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Err(err) => eprintln!("alert: could not run curl: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(n_restarts: u64, alerting: bool) -> Snapshot {
        Snapshot { n_restarts, alerting }
    }

    #[test]
    fn first_sweep_seeds_without_alerting() {
        let d = decide(None, "failed", 9, true, "x");
        assert!(d.notify.is_none());
        assert!(!d.alerting);
    }

    #[test]
    fn alerts_on_transition_to_failed() {
        let prev = snap(0, false);
        let d = decide(Some(&prev), "failed", 0, false, "blog");
        assert_eq!(d.notify.unwrap().0, "rotating_light");
        assert!(d.alerting);
    }

    #[test]
    fn alerts_on_rapid_restarts() {
        let prev = snap(3, false);
        let d = decide(Some(&prev), "activating", 6, false, "blog");
        assert_eq!(d.notify.unwrap().0, "arrows_counterclockwise");
        assert!(d.alerting);
    }

    #[test]
    fn single_restart_stays_quiet() {
        let prev = snap(3, false);
        let d = decide(Some(&prev), "active", 4, false, "blog");
        assert!(d.notify.is_none());
        assert!(!d.alerting);
    }

    #[test]
    fn crash_loop_debounced_to_one_alert() {
        // Already alerting; restarts keep climbing — no second alert.
        let prev = snap(6, true);
        let d = decide(Some(&prev), "failed", 12, false, "blog");
        assert!(d.notify.is_none());
        assert!(d.alerting, "still in the trouble episode");
    }

    #[test]
    fn recovers_once_stable() {
        let prev = snap(12, true);
        let d = decide(Some(&prev), "active", 12, false, "blog");
        assert_eq!(d.notify.unwrap().0, "white_check_mark");
        assert!(!d.alerting);
    }

    #[test]
    fn stopped_does_not_count_as_recovered() {
        // A manual stop (inactive) shouldn't fire a "recovered" notification.
        let prev = snap(12, true);
        let d = decide(Some(&prev), "inactive", 12, false, "blog");
        assert!(d.notify.is_none());
        assert!(d.alerting);
    }
}
