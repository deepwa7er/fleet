//! Thin wrapper over `systemctl` and `journalctl`.
//!
//! Every function takes a unit name that the caller has already validated
//! against the configured allowlist (see [`crate::config::Config::find_unit`]).
//! Commands are invoked with explicit argument vectors via [`tokio::process`] —
//! never through a shell — so a unit name is passed as a single argv entry and
//! cannot inject additional commands.

use std::process::Stdio;

use anyhow::{Context, bail};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

/// systemd reports an unknown/unlimited memory figure as `u64::MAX`.
const MEMORY_UNSET: u64 = u64::MAX;

/// The systemd properties we read for a service's status line.
const STATUS_PROPERTIES: &str =
    "ActiveState,SubState,ActiveEnterTimestamp,MemoryCurrent,MainPID,Description";

/// journald fields requested for log queries — keeps the payload small.
const LOG_FIELDS: &str = "__REALTIME_TIMESTAMP,PRIORITY,MESSAGE";

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub unit: String,
    pub name: String,
    /// `active`, `inactive`, `failed`, `activating`, … (systemd `ActiveState`).
    pub active_state: String,
    /// `running`, `dead`, `exited`, … (systemd `SubState`).
    pub sub_state: String,
    pub description: String,
    /// Human-readable "active since" timestamp, or `None` if not running.
    pub since: Option<String>,
    pub memory_bytes: Option<u64>,
    pub pid: Option<u32>,
}

/// A lifecycle action that may be performed on a service. The set is
/// deliberately small — these are the only verbs the polkit grant permits.
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

    fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LogEntry {
    /// Wall-clock time in microseconds since the Unix epoch.
    pub timestamp_us: u64,
    /// syslog priority 0 (emerg) – 7 (debug).
    pub priority: u8,
    pub message: String,
}

/// Query the current status of a single unit.
pub async fn status(unit: &str, name: &str) -> anyhow::Result<ServiceStatus> {
    let output = Command::new("systemctl")
        .args(["show", unit, "--no-pager"])
        .arg(format!("--property={STATUS_PROPERTIES}"))
        .output()
        .await
        .context("running systemctl show")?;

    // `systemctl show` exits 0 and prints empty values for unknown units, so we
    // parse whatever it returned rather than treating a nonzero exit as fatal.
    let text = String::from_utf8_lossy(&output.stdout);
    let props = parse_properties(&text);

    let get = |key: &str| props.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);

    let pid = get("MainPID")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&p| p != 0);

    let memory_bytes = get("MemoryCurrent")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&m| m != MEMORY_UNSET);

    let since = get("ActiveEnterTimestamp")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok(ServiceStatus {
        unit: unit.to_owned(),
        name: name.to_owned(),
        active_state: get("ActiveState").unwrap_or("unknown").to_owned(),
        sub_state: get("SubState").unwrap_or("unknown").to_owned(),
        description: get("Description").unwrap_or(name).to_owned(),
        since,
        memory_bytes,
        pid,
    })
}

/// Start, stop, or restart a unit.
///
/// `systemctl` talks to systemd over D-Bus; for a non-root caller the action is
/// authorized by polkit. The deploy-installed polkit rule grants the
/// `lighthouse` user exactly these verbs on exactly the configured units, so no
/// root, sudo, or setuid is involved (it works under `NoNewPrivileges=true`).
pub async fn control(unit: &str, action: ServiceAction) -> anyhow::Result<()> {
    let verb = action.verb();
    let output = Command::new("systemctl")
        .args([verb, unit])
        .output()
        .await
        .with_context(|| format!("running systemctl {verb}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("systemctl {verb} {unit} failed: {}", stderr.trim());
    }
    Ok(())
}

/// Fetch the most recent `lines` log entries for a unit.
pub async fn recent_logs(unit: &str, lines: u32) -> anyhow::Result<Vec<LogEntry>> {
    let output = Command::new("journalctl")
        .args(["-u", unit, "-o", "json", "--no-pager"])
        .arg("-n")
        .arg(lines.to_string())
        .arg(format!("--output-fields={LOG_FIELDS}"))
        .output()
        .await
        .context("running journalctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("journalctl failed: {}", stderr.trim());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_log_line).collect())
}

/// Number of log entries buffered between the journalctl reader and the SSE
/// client. A slow client applies backpressure rather than growing memory.
const FOLLOW_BUFFER: usize = 64;

/// Spawn `journalctl -f` and stream new log entries as they arrive.
///
/// A dedicated task owns the child: it forwards parsed entries over a channel
/// and, when the stream ends — either the SSE client disconnects (the receiver
/// drops, so sends fail) or journalctl exits — it kills the child and `wait()`s
/// on it, so the process is reaped and no zombie is left behind.
pub fn follow_logs(unit: &str) -> anyhow::Result<impl Stream<Item = LogEntry> + use<>> {
    let mut child = Command::new("journalctl")
        .args(["-u", unit, "-f", "-n", "0", "-o", "json", "--no-pager"])
        .arg(format!("--output-fields={LOG_FIELDS}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawning journalctl -f")?;

    let stdout = child
        .stdout
        .take()
        .context("journalctl stdout was not captured")?;
    let mut lines = BufReader::new(stdout).lines();

    let (tx, rx) = tokio::sync::mpsc::channel::<LogEntry>(FOLLOW_BUFFER);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // The SSE client disconnected (receiver dropped) — even if the
                // service is quiet and no new line ever arrives, stop promptly.
                () = tx.closed() => break,
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Some(entry) = parse_log_line(&line)
                            && tx.send(entry).await.is_err()
                        {
                            break;
                        }
                    }
                    // journalctl exited or errored.
                    _ => break,
                },
            }
        }
        // Kill and reap the child so it never lingers (as a process or zombie).
        let _ = child.start_kill();
        let _ = child.wait().await;
    });

    Ok(ReceiverStream::new(rx))
}

/// Parse `systemctl show` `key=value` output into borrowed pairs.
fn parse_properties(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

/// Parse one line of journald JSON output into a [`LogEntry`].
fn parse_log_line(line: &str) -> Option<LogEntry> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;

    let timestamp_us = value
        .get("__REALTIME_TIMESTAMP")
        .and_then(journal_string)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let priority = value
        .get("PRIORITY")
        .and_then(journal_string)
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(6); // default to LOG_INFO

    let message = value
        .get("MESSAGE")
        .map(journal_message)
        .unwrap_or_default();

    Some(LogEntry {
        timestamp_us,
        priority,
        message,
    })
}

/// journald scalar fields are emitted as JSON strings.
fn journal_string(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// A journald `MESSAGE` is normally a string, but non-UTF-8 messages are encoded
/// as an array of byte values. Handle both so binary log lines don't vanish.
fn journal_message(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(bytes) => {
            let raw: Vec<u8> = bytes
                .iter()
                .filter_map(|b| b.as_u64())
                .map(|b| b as u8)
                .collect();
            String::from_utf8_lossy(&raw).into_owned()
        }
        _ => String::new(),
    }
}
