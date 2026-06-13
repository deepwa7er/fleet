//! Docker backend: status, logs, and control for monitored containers.
//!
//! Lighthouse never touches the raw Docker socket. It speaks the Docker Engine
//! HTTP API to a **socket-proxy** (configured `docker.proxy_url`) that exposes
//! only the container inspect / logs / start / stop / restart endpoints — the
//! same least-privilege posture the polkit grant gives the systemd side.
//!
//! Container state is mapped onto systemd's `active_state`/`sub_state`
//! vocabulary so the API and frontend stay source-agnostic.

use anyhow::{Context, bail};
use reqwest::StatusCode;
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio_stream::{Stream, StreamExt};
use tokio_stream::wrappers::ReceiverStream;

use crate::model::{LogEntry, ServiceAction, ServiceStatus, Source};

/// syslog-style priorities we tag Docker output with, so the viewer can colour
/// stderr like an error and stdout like info.
const PRIORITY_STDOUT: u8 = 6;
const PRIORITY_STDERR: u8 = 3;

/// Docker multiplexes stdout/stderr with an 8-byte frame header when the
/// container has no TTY: `[stream, 0, 0, 0, size:u32 big-endian]`.
const FRAME_HEADER_LEN: usize = 8;

/// `GET /containers/{id}/json` — the subset of the inspect payload we use.
#[derive(Debug, Deserialize)]
struct Inspect {
    #[serde(rename = "Config")]
    config: InspectConfig,
    #[serde(rename = "State")]
    state: InspectState,
}

#[derive(Debug, Deserialize)]
struct InspectConfig {
    #[serde(rename = "Tty", default)]
    tty: bool,
    #[serde(rename = "Image", default)]
    image: String,
}

#[derive(Debug, Deserialize)]
struct InspectState {
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "ExitCode", default)]
    exit_code: i64,
    #[serde(rename = "StartedAt", default)]
    started_at: String,
    #[serde(rename = "Pid", default)]
    pid: u32,
    #[serde(rename = "Health")]
    health: Option<InspectHealth>,
}

#[derive(Debug, Deserialize)]
struct InspectHealth {
    #[serde(rename = "Status", default)]
    status: String,
}

/// Trim a trailing slash so `format!` joins produce a single separator.
fn base(proxy_url: &str) -> &str {
    proxy_url.trim_end_matches('/')
}

/// Inspect a container, or `None` if it does not exist (404).
async fn inspect(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
) -> anyhow::Result<Option<Inspect>> {
    let url = format!("{}/containers/{container}/json", base(proxy_url));
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("inspecting container {container} via the docker proxy"))?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .with_context(|| format!("docker proxy rejected inspect of {container}"))?;
    let inspect = response
        .json::<Inspect>()
        .await
        .with_context(|| format!("parsing inspect of {container}"))?;
    Ok(Some(inspect))
}

/// Current status of a configured container. A container that doesn't exist
/// (e.g. removed) is reported as inactive/absent rather than erroring, so one
/// missing container never breaks the whole dashboard listing.
pub async fn status(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
    name: &str,
) -> anyhow::Result<ServiceStatus> {
    let Some(inspect) = inspect(client, proxy_url, container).await? else {
        return Ok(ServiceStatus {
            source: Source::Docker,
            id: container.to_owned(),
            name: name.to_owned(),
            active_state: "inactive".to_owned(),
            sub_state: "absent".to_owned(),
            description: "container does not exist".to_owned(),
            since: None,
            memory_bytes: None,
            pid: None,
        });
    };

    let (active_state, sub_state) = map_state(&inspect.state);
    let running = inspect.state.status == "running";

    Ok(ServiceStatus {
        source: Source::Docker,
        id: container.to_owned(),
        name: name.to_owned(),
        active_state,
        sub_state,
        description: inspect.config.image.clone(),
        since: running
            .then(|| inspect.state.started_at.trim().to_owned())
            .filter(|s| !s.is_empty() && !s.starts_with("0001-")),
        // The Docker API exposes memory only via a separate streaming /stats
        // call; not worth its cost for a status line.
        memory_bytes: None,
        pid: (inspect.state.pid != 0).then_some(inspect.state.pid),
    })
}

/// Map a Docker container state onto systemd's vocabulary so the frontend's
/// status styling works uniformly across sources.
fn map_state(state: &InspectState) -> (String, String) {
    let active = match state.status.as_str() {
        "running" | "paused" => "active",
        "restarting" => "activating",
        "created" => "inactive",
        "exited" if state.exit_code != 0 => "failed",
        "exited" => "inactive",
        "dead" => "failed",
        _ => "inactive",
    };
    // Prefer the health verdict for a running container; otherwise the raw
    // Docker status is the most informative sub-state.
    let sub = match (&state.health, state.status.as_str()) {
        (Some(health), "running") if !health.status.is_empty() => health.status.clone(),
        _ => state.status.clone(),
    };
    (active.to_owned(), sub)
}

/// Start, stop, or restart a container via the proxy. A 304 ("already in that
/// state") counts as success.
pub async fn control(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
    action: ServiceAction,
) -> anyhow::Result<()> {
    let verb = action.verb();
    let url = format!("{}/containers/{container}/{verb}", base(proxy_url));
    let response = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("running docker {verb} on {container}"))?;

    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    bail!("docker {verb} {container} failed: {status} {}", body.trim());
}

/// Fetch the most recent `lines` log entries for a container.
pub async fn recent_logs(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
    lines: u32,
) -> anyhow::Result<Vec<LogEntry>> {
    let tty = is_tty(client, proxy_url, container).await?;
    let url = format!(
        "{}/containers/{container}/logs?stdout=1&stderr=1&timestamps=1&tail={lines}",
        base(proxy_url)
    );
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("reading logs for {container}"))?
        .error_for_status()
        .with_context(|| format!("docker proxy rejected logs of {container}"))?;
    let body = response.bytes().await.context("reading docker log body")?;

    let mut decoder = FrameDecoder::new(tty);
    let mut entries = decoder.push(&body);
    entries.extend(decoder.flush());
    Ok(entries)
}

/// Number of entries buffered between the log reader task and the SSE client.
const FOLLOW_BUFFER: usize = 64;

/// Stream new log entries as they arrive (`follow=1`).
///
/// A dedicated task owns the upstream HTTP body: it forwards decoded entries
/// over a channel and stops as soon as either the SSE client disconnects (the
/// receiver drops) or the upstream stream ends. Dropping the response aborts
/// the request, so no follow connection lingers.
pub async fn follow_logs(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
) -> anyhow::Result<impl Stream<Item = LogEntry> + use<>> {
    let tty = is_tty(client, proxy_url, container).await?;
    let url = format!(
        "{}/containers/{container}/logs?stdout=1&stderr=1&timestamps=1&follow=1&tail=0",
        base(proxy_url)
    );
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("following logs for {container}"))?
        .error_for_status()
        .with_context(|| format!("docker proxy rejected log follow of {container}"))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<LogEntry>(FOLLOW_BUFFER);
    tokio::spawn(async move {
        let mut decoder = FrameDecoder::new(tty);
        let mut body = response.bytes_stream();
        loop {
            tokio::select! {
                () = tx.closed() => break,
                chunk = body.next() => match chunk {
                    Some(Ok(bytes)) => {
                        for entry in decoder.push(&bytes) {
                            if tx.send(entry).await.is_err() {
                                return;
                            }
                        }
                    }
                    // Stream ended or errored.
                    _ => break,
                },
            }
        }
    });

    Ok(ReceiverStream::new(rx))
}

/// Whether the container allocates a TTY, which determines its log framing.
async fn is_tty(
    client: &reqwest::Client,
    proxy_url: &str,
    container: &str,
) -> anyhow::Result<bool> {
    Ok(inspect(client, proxy_url, container)
        .await?
        .map(|i| i.config.tty)
        .unwrap_or(false))
}

/// Decodes a Docker log byte stream into [`LogEntry`]s, handling both the
/// 8-byte multiplexing frame header (no TTY) and raw output (TTY), and
/// buffering bytes that span chunk boundaries.
struct FrameDecoder {
    tty: bool,
    buf: Vec<u8>,
}

impl FrameDecoder {
    fn new(tty: bool) -> Self {
        Self { tty, buf: Vec::new() }
    }

    /// Feed bytes, returning entries for every complete log line now available.
    fn push(&mut self, bytes: &[u8]) -> Vec<LogEntry> {
        self.buf.extend_from_slice(bytes);
        let mut entries = Vec::new();
        if self.tty {
            // No framing: emit complete (newline-terminated) lines.
            while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                push_line(&mut entries, &line[..line.len() - 1], PRIORITY_STDOUT);
            }
            return entries;
        }
        // Multiplexed: consume whole frames only.
        loop {
            if self.buf.len() < FRAME_HEADER_LEN {
                break;
            }
            let size = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]])
                as usize;
            if self.buf.len() < FRAME_HEADER_LEN + size {
                break;
            }
            let priority = if self.buf[0] == 2 { PRIORITY_STDERR } else { PRIORITY_STDOUT };
            let frame: Vec<u8> = self.buf.drain(..FRAME_HEADER_LEN + size).collect();
            for line in frame[FRAME_HEADER_LEN..].split(|&b| b == b'\n') {
                push_line(&mut entries, line, priority);
            }
        }
        entries
    }

    /// Emit any buffered remainder as a final line (the response ended without
    /// a trailing newline). Only meaningful for a complete, non-follow body.
    fn flush(&mut self) -> Vec<LogEntry> {
        let mut entries = Vec::new();
        if self.tty {
            let rest = std::mem::take(&mut self.buf);
            push_line(&mut entries, &rest, PRIORITY_STDOUT);
        }
        entries
    }
}

/// Parse one decoded log line — `<rfc3339-timestamp> <message>` when timestamps
/// are enabled — into a [`LogEntry`]. Empty lines are skipped.
fn push_line(entries: &mut Vec<LogEntry>, raw: &[u8], priority: u8) {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches('\r');
    if text.is_empty() {
        return;
    }
    let (timestamp_us, message) = match text.split_once(' ') {
        Some((ts, rest)) => match parse_timestamp(ts) {
            Some(us) => (us, rest.to_owned()),
            None => (0, text.to_owned()),
        },
        None => (0, text.to_owned()),
    };
    entries.push(LogEntry { timestamp_us, priority, message });
}

/// Parse an RFC 3339 (nanosecond) timestamp into microseconds since the epoch.
fn parse_timestamp(value: &str) -> Option<u64> {
    let dt = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    u64::try_from(dt.unix_timestamp_nanos() / 1_000).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a non-TTY multiplexed frame: header + payload.
    fn frame(stream: u8, payload: &str) -> Vec<u8> {
        let mut v = vec![stream, 0, 0, 0];
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload.as_bytes());
        v
    }

    #[test]
    fn decodes_multiplexed_frames_with_timestamps() {
        let mut dec = FrameDecoder::new(false);
        let mut bytes = frame(1, "2026-06-13T18:00:00.000000000Z hello stdout\n");
        bytes.extend(frame(2, "2026-06-13T18:00:01.000000000Z oops stderr\n"));
        let entries = dec.push(&bytes);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "hello stdout");
        assert_eq!(entries[0].priority, PRIORITY_STDOUT);
        assert_eq!(entries[1].message, "oops stderr");
        assert_eq!(entries[1].priority, PRIORITY_STDERR);
        assert!(entries[0].timestamp_us > 0);
        assert!(entries[1].timestamp_us > entries[0].timestamp_us);
    }

    #[test]
    fn buffers_frame_split_across_chunks() {
        let full = frame(1, "2026-06-13T18:00:00.000000000Z split line\n");
        let (head, tail) = full.split_at(6);
        let mut dec = FrameDecoder::new(false);
        assert!(dec.push(head).is_empty(), "partial header yields nothing yet");
        let entries = dec.push(tail);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "split line");
    }

    #[test]
    fn tty_stream_splits_on_newlines() {
        let mut dec = FrameDecoder::new(true);
        let entries = dec.push(b"2026-06-13T18:00:00.000000000Z one\n2026-06-13T18:00:01.000000000Z two\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "one");
        assert_eq!(entries[1].message, "two");
    }

    #[test]
    fn flush_emits_unterminated_tty_remainder() {
        let mut dec = FrameDecoder::new(true);
        assert!(dec.push(b"2026-06-13T18:00:00.000000000Z no newline").is_empty());
        let entries = dec.flush();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "no newline");
    }

    #[test]
    fn line_without_timestamp_keeps_whole_message() {
        let mut entries = Vec::new();
        push_line(&mut entries, b"plainmessage", PRIORITY_STDOUT);
        assert_eq!(entries[0].message, "plainmessage");
        assert_eq!(entries[0].timestamp_us, 0);
    }
}
