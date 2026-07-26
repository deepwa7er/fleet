//! Per-request access records — the fleet's only source of "who actually uses
//! what".
//!
//! Every request that reaches [`crate::proxy::handle`] produces one [`Record`],
//! emitted as a single JSON line on stdout and therefore captured by journald
//! alongside breakwater's other output. Lines carry a `"kind"` discriminator so
//! a consumer can pick them out of the plain-text startup/error lines the rest
//! of the binary prints (`journalctl -u breakwater -o cat | grep '"kind":"access"'`).
//!
//! Two properties matter more than completeness, because this sits in the path
//! of the fleet's front door:
//!
//! 1. **Recording never blocks a request.** Records go to a bounded channel via
//!    `try_send` and are written by one background task. Serializing and writing
//!    to the journal socket happens on that task, never on the connection task.
//! 2. **Recording never applies backpressure.** If the channel is full — a burst
//!    faster than the journal drains — the record is dropped, not awaited. A lost
//!    log line is always cheaper than a delayed request.
//!
//! Dropped records are counted and reported (`"kind":"access_dropped"`) rather
//! than silently swallowed, so a gap in the data is always visible as a gap.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufWriter, Stdout};
use tokio::sync::mpsc;

/// Records buffered between the connection tasks and the writer. Sized to absorb
/// a burst without ever making a request wait; beyond it, records are dropped
/// (and counted) instead.
const CHANNEL_CAPACITY: usize = 1024;

/// Records written per flush of the output buffer.
const BATCH: usize = 64;

/// How the request was resolved — the routing decision, independent of the HTTP
/// status that resulted from it.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// Forwarded to a local upstream service.
    Proxy,
    /// Served from a static directory (e.g. the docs site).
    Static,
    /// No route matched the `Host` — a 404 from breakwater itself.
    Miss,
}

/// One completed request.
#[derive(Debug, Serialize)]
pub struct Record {
    /// Always `"access"`. Lets a consumer separate these from breakwater's
    /// plain-text journal lines without parsing every line as JSON.
    kind: &'static str,
    /// Unix epoch milliseconds at which the response head was ready.
    at_ms: u64,
    route: Route,
    /// The `Host` header as sent by the client, lowercased and without any port.
    /// Empty when the client sent no `Host`.
    host: String,
    method: String,
    /// Request path, without the query string.
    path: String,
    /// Query string, when present — kept separate from `path` so a consumer can
    /// aggregate by path alone, or read search terms, as it chooses.
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    status: u16,
    /// Milliseconds from the start of routing until the response *head* was
    /// ready. Deliberately NOT the full response duration: bodies stream (SSE
    /// log tails, deploy consoles) and can outlive the head by minutes, so a
    /// body-inclusive number would measure how long a client stayed connected
    /// rather than how fast the fleet answered.
    ms: u64,
    client_ip: String,
    /// Present when the client sent one. `lighthouse`'s reachability probe sets
    /// a distinctive agent so synthetic monitoring traffic — which hits every
    /// routed host on an interval — can be excluded when asking what a *person*
    /// used.
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
}

impl Record {
    /// Build a record, stamping it with the current wall-clock time.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        route: Route,
        host: String,
        method: String,
        path: String,
        query: Option<String>,
        status: u16,
        ms: u64,
        client_ip: String,
        user_agent: Option<String>,
    ) -> Self {
        Self {
            kind: "access",
            at_ms: now_ms(),
            route,
            host,
            method,
            path,
            query,
            status,
            ms,
            client_ip,
            user_agent,
        }
    }
}

/// Milliseconds since the Unix epoch. A clock before the epoch is not a
/// condition worth failing a request over, so it records as 0.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The handle held by connection tasks. Cloning is cheap (an `mpsc::Sender` and
/// an `Arc`), so each connection can hold its own.
#[derive(Clone)]
pub struct Recorder {
    tx: mpsc::Sender<Record>,
    dropped: Arc<AtomicU64>,
}

impl Recorder {
    /// Submit a record. Returns immediately, always — on a full (or closed)
    /// channel the record is counted as dropped and discarded.
    pub fn record(&self, record: Record) {
        if self.tx.try_send(record).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Start the writer task and return the handle used to submit records.
pub fn spawn() -> Recorder {
    let (tx, mut rx) = mpsc::channel::<Record>(CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let counter = dropped.clone();

    tokio::spawn(async move {
        let mut out = BufWriter::new(tokio::io::stdout());
        let mut batch = Vec::with_capacity(BATCH);
        let mut reported = 0u64;

        // `recv_many` returns 0 only once the channel is closed and drained,
        // which happens at shutdown when the last `Recorder` is gone.
        while rx.recv_many(&mut batch, BATCH).await > 0 {
            let total = counter.load(Ordering::Relaxed);
            if total > reported {
                let line = format!(
                    r#"{{"kind":"access_dropped","count":{}}}"#,
                    total - reported
                );
                if write_line(&mut out, &line).await.is_ok() {
                    reported = total;
                }
            }

            for record in batch.drain(..) {
                match serde_json::to_string(&record) {
                    Ok(line) => {
                        if write_line(&mut out, &line).await.is_err() {
                            return;
                        }
                    }
                    // Every field is a plain string or integer, so this is
                    // unreachable in practice; drop the record rather than
                    // taking down the writer for the rest of the process.
                    Err(err) => eprintln!("breakwater: access record not serializable: {err}"),
                }
            }

            if out.flush().await.is_err() {
                return;
            }
        }
    });

    Recorder { tx, dropped }
}

/// Write one line. A failure here means stdout is gone (the journal socket
/// closed), which the caller treats as the end of the writer's life — retrying
/// into a dead pipe every request would be worse than going quiet.
async fn write_line(out: &mut BufWriter<Stdout>, line: &str) -> std::io::Result<()> {
    out.write_all(line.as_bytes()).await?;
    out.write_all(b"\n").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(query: Option<&str>, user_agent: Option<&str>) -> Record {
        Record::new(
            Route::Proxy,
            "spyglass.intern.deepwa7er.net".to_string(),
            "GET".to_string(),
            "/search".to_string(),
            query.map(str::to_string),
            200,
            12,
            "100.98.184.58".to_string(),
            user_agent.map(str::to_string),
        )
    }

    fn json(record: &Record) -> serde_json::Value {
        serde_json::from_str(&serde_json::to_string(record).unwrap()).unwrap()
    }

    #[test]
    fn serializes_the_full_shape() {
        let value = json(&sample(Some("q=axum"), Some("Mozilla/5.0")));
        assert_eq!(value["kind"], "access");
        assert_eq!(value["route"], "proxy");
        assert_eq!(value["host"], "spyglass.intern.deepwa7er.net");
        assert_eq!(value["method"], "GET");
        assert_eq!(value["path"], "/search");
        assert_eq!(value["query"], "q=axum");
        assert_eq!(value["status"], 200);
        assert_eq!(value["ms"], 12);
        assert_eq!(value["client_ip"], "100.98.184.58");
        assert_eq!(value["user_agent"], "Mozilla/5.0");
        assert!(value["at_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn omits_absent_optional_fields() {
        let value = json(&sample(None, None));
        assert!(value.get("query").is_none());
        assert!(value.get("user_agent").is_none());
    }

    #[test]
    fn each_record_is_exactly_one_line() {
        // The journal frames these by newline, so an embedded newline (a path or
        // user-agent containing one) would split a record into two unparseable
        // halves. serde_json escapes it; this pins that.
        let record = Record::new(
            Route::Miss,
            "h".to_string(),
            "GET".to_string(),
            "/a\nb".to_string(),
            None,
            404,
            0,
            "127.0.0.1".to_string(),
            Some("bad\nagent".to_string()),
        );
        let line = serde_json::to_string(&record).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(json(&record)["path"], "/a\nb");
    }

    #[test]
    fn route_names_are_stable() {
        // These strings are the query surface for anything reading the log.
        assert_eq!(serde_json::to_string(&Route::Proxy).unwrap(), r#""proxy""#);
        assert_eq!(
            serde_json::to_string(&Route::Static).unwrap(),
            r#""static""#
        );
        assert_eq!(serde_json::to_string(&Route::Miss).unwrap(), r#""miss""#);
    }

    #[tokio::test]
    async fn dropping_records_never_blocks_the_caller() {
        // A recorder whose writer never runs: fill it past capacity and confirm
        // every call returns and the overflow is counted, not awaited.
        let (tx, _rx) = mpsc::channel::<Record>(2);
        let recorder = Recorder {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        for _ in 0..10 {
            recorder.record(sample(None, None));
        }
        assert_eq!(recorder.dropped.load(Ordering::Relaxed), 8);
    }
}
