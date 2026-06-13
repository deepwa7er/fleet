//! Types shared across the monitoring backends.
//!
//! A monitored entity is either a systemd unit or a Docker container. Both
//! backends ([`crate::systemd`], [`crate::docker`]) produce the same
//! [`ServiceStatus`] / [`LogEntry`] shapes and accept the same
//! [`ServiceAction`], so the API layer and frontend treat them uniformly —
//! the only difference a caller sees is the [`Source`] discriminator.

use serde::{Deserialize, Serialize};

/// Which backend a service is managed by. Serialized lowercase (`"systemd"`,
/// `"docker"`) and used as the `{source}` path segment in the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Systemd,
    Docker,
}

impl Source {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "systemd" => Some(Self::Systemd),
            "docker" => Some(Self::Docker),
            _ => None,
        }
    }
}

/// A monitored service's current status. Field meanings are normalized across
/// backends: a Docker container's state is mapped onto the same vocabulary
/// systemd uses (`active`/`inactive`/`failed`/`activating`) so the frontend's
/// status styling is source-agnostic.
#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub source: Source,
    /// Stable identifier within the source: a unit name (`ferry.service`) or a
    /// container name (`navidrome`).
    pub id: String,
    /// Human-readable display label.
    pub name: String,
    /// `active`, `inactive`, `failed`, `activating`, …
    pub active_state: String,
    /// Finer-grained state: `running`, `dead`, `exited`, `restarting`, …
    pub sub_state: String,
    pub description: String,
    /// Human-readable "running since" value, or `None` if not running.
    pub since: Option<String>,
    pub memory_bytes: Option<u64>,
    pub pid: Option<u32>,
}

/// One log line, normalized across journald and Docker log streams.
#[derive(Debug, Serialize)]
pub struct LogEntry {
    /// Wall-clock time in microseconds since the Unix epoch (0 if unknown).
    pub timestamp_us: u64,
    /// syslog-style priority 0 (emerg) – 7 (debug). Docker stdout maps to 6
    /// (info) and stderr to 3 (err) so the viewer can colour them.
    pub priority: u8,
    pub message: String,
}

/// A lifecycle action. The set is deliberately small — exactly the verbs both
/// the polkit grant (systemd) and the socket-proxy allowlist (Docker) permit.
#[derive(Debug, Clone, Copy)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}
