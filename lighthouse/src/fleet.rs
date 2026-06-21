//! Reference data about each service, loaded from the docs site's generated
//! `fleet.json` (produced by `tugboat fleet docs`) and merged onto live status.
//!
//! This is what turns the dashboard into the fleet "home": alongside "is it up",
//! each service can show what it *is* (description), where to reach it (public
//! URL), and where its docs live. Best-effort — if `fleet.json` is missing or
//! unreadable the dashboard simply shows live status, exactly as before, so the
//! dashboard never depends on the docs site being deployed.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The reference fields merged onto a service's live status.
#[derive(Debug, Clone, Serialize)]
pub struct FleetRef {
    /// One-line description — the docs' authoritative service summary.
    pub summary: String,
    /// Public URL, when the service is routed (the card's "open" link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Loopback port the service listens on, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Git remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Deep link to the service's entry on the docs site (the "docs" link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
}

/// Top level of `fleet.json` — only the fields the dashboard consumes.
#[derive(Deserialize)]
struct FleetFile {
    #[serde(default)]
    docs_url: Option<String>,
    #[serde(default)]
    services: Vec<FleetService>,
}

#[derive(Deserialize)]
struct FleetService {
    name: String,
    #[serde(default)]
    description: String,
    /// The systemd unit, present for tugboat-deployed services — the join key.
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    repo: Option<String>,
}

/// Load `fleet.json` and index it by systemd unit. Best-effort: any problem
/// (missing file, parse error) yields an empty map, and only services that carry
/// a `unit` can be matched to what lighthouse monitors.
pub fn load(path: &Path) -> HashMap<String, FleetRef> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(file) = serde_json::from_str::<FleetFile>(&text) else {
        return HashMap::new();
    };

    let mut by_unit = HashMap::new();
    for service in file.services {
        let Some(unit) = service.unit else { continue };
        let docs_url = file
            .docs_url
            .as_ref()
            .map(|base| format!("{}/#{}", base.trim_end_matches('/'), service.name));
        by_unit.insert(
            unit,
            FleetRef {
                summary: service.description.trim().to_owned(),
                url: service.url,
                port: service.port,
                repo: service.repo,
                docs_url,
            },
        );
    }
    by_unit
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_json(contents: &str) -> std::path::PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("lh-fleet-{}-{n}.json", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn indexes_by_unit_and_builds_docs_link() {
        let path = temp_json(
            r#"{
              "docs_url": "https://docs.internal.deepwa7er.com",
              "services": [
                {"name":"ferry","description":"redirects","unit":"ferry.service",
                 "url":"https://ferry.internal.deepwa7er.com","port":7777,
                 "repo":"git@github.com:x/ferry.git"},
                {"name":"pilot","description":"the docs site"}
              ]
            }"#,
        );
        let map = load(&path);
        std::fs::remove_file(&path).ok();

        // `pilot` has no unit, so it can't be joined to a monitored service.
        assert_eq!(map.len(), 1);
        let ferry = map.get("ferry.service").expect("ferry indexed by unit");
        assert_eq!(ferry.summary, "redirects");
        assert_eq!(ferry.url.as_deref(), Some("https://ferry.internal.deepwa7er.com"));
        assert_eq!(ferry.port, Some(7777));
        assert_eq!(
            ferry.docs_url.as_deref(),
            Some("https://docs.internal.deepwa7er.com/#ferry"),
        );
    }

    #[test]
    fn missing_file_is_empty_not_an_error() {
        assert!(load(std::path::Path::new("/nonexistent/fleet.json")).is_empty());
    }

    #[test]
    fn no_docs_url_means_no_docs_link() {
        let path =
            temp_json(r#"{"services":[{"name":"ferry","description":"x","unit":"ferry.service"}]}"#);
        let map = load(&path);
        std::fs::remove_file(&path).ok();
        assert!(map.get("ferry.service").unwrap().docs_url.is_none());
    }
}
