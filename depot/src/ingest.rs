//! Pulling breakwater's access log out of journald.
//!
//! breakwater emits one JSON line per request to stdout, which systemd captures.
//! depot runs on the same host, so ingest is a periodic `journalctl` read rather
//! than a network hop — there is no delivery to fail, and the journal is the
//! buffer if depot is down.
//!
//! **Position is journald's to track, not ours.** `journalctl --cursor-file`
//! resumes after the last entry it handed us and rewrites the file on exit, so
//! depot never reasons about timestamps or re-reads a window it already saw. If
//! the cursor file is missing (first run, or it was deleted) journalctl starts
//! from the beginning of the journal, which backfills everything still retained
//! — safe to do at any time, because inserts are `INSERT OR IGNORE` on a natural
//! key.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::store::{AccessRecord, Store};

/// Floor on the poll interval, so a misconfigured tiny value can't spawn
/// `journalctl` in a hot loop.
const MIN_INTERVAL_SECS: u64 = 10;

pub struct Config {
    /// systemd unit to read. Normally `breakwater`.
    pub unit: String,
    /// Where journald records our read position.
    pub cursor_file: PathBuf,
    pub interval: Duration,
}

/// One ingest pass' outcome.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    /// Records parsed out of the journal.
    pub seen: usize,
    /// Records that were new (the rest were already stored).
    pub stored: usize,
    /// Requests breakwater itself dropped rather than delay — reported by its
    /// `access_dropped` markers. These are gaps in the data that no amount of
    /// re-reading recovers, so they are surfaced, never silently passed over.
    pub dropped_upstream: u64,
    /// Lines that were neither valid access records nor drop markers. Expected
    /// and harmless: breakwater's plain-text startup and error lines share the
    /// journal with its JSON.
    pub skipped: usize,
}

/// Run the ingest loop until cancelled.
pub async fn run(store: Arc<Store>, config: Config) {
    let interval = config.interval.max(Duration::from_secs(MIN_INTERVAL_SECS));
    tracing::info!(
        unit = %config.unit,
        interval_secs = interval.as_secs(),
        cursor = %config.cursor_file.display(),
        "access-log ingest started"
    );
    loop {
        match once(&store, &config).await {
            Ok(pass) => {
                if pass.stored > 0 || pass.dropped_upstream > 0 {
                    tracing::info!(
                        seen = pass.seen,
                        stored = pass.stored,
                        dropped_upstream = pass.dropped_upstream,
                        "ingested access records"
                    );
                }
                if pass.dropped_upstream > 0 {
                    tracing::warn!(
                        count = pass.dropped_upstream,
                        "breakwater dropped access records under load — \
                         these requests are permanently absent from the warehouse"
                    );
                }
            }
            // Never fatal: a failed read is retried on the next tick, and the
            // cursor file means nothing is lost by waiting.
            Err(err) => tracing::error!(error = %err, "access-log ingest failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// One pass: read everything since the cursor and store it.
pub async fn once(store: &Store, config: &Config) -> Result<Pass, String> {
    if let Some(parent) = config.cursor_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let output = Command::new("journalctl")
        .arg("-u")
        .arg(&config.unit)
        .arg("-o")
        .arg("cat")
        .arg("--no-pager")
        .arg(format!("--cursor-file={}", config.cursor_file.display()))
        .output()
        .await
        .map_err(|e| format!("running journalctl: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "journalctl exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let parsed = parse(&text);
    let stored = store
        .insert_access(&parsed.records)
        .map_err(|e| format!("storing access records: {e}"))?;

    Ok(Pass {
        seen: parsed.records.len(),
        stored,
        dropped_upstream: parsed.dropped_upstream,
        skipped: parsed.skipped,
    })
}

struct Parsed {
    records: Vec<AccessRecord>,
    dropped_upstream: u64,
    skipped: usize,
}

/// Pull access records and drop markers out of a journal chunk.
///
/// Lines that are not our JSON are skipped rather than failing the pass —
/// breakwater's plain-text output shares the same stream, and one malformed line
/// must never block every well-formed one behind it.
fn parse(text: &str) -> Parsed {
    let mut records = Vec::new();
    let mut dropped_upstream = 0u64;
    let mut skipped = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            skipped += 1;
            continue;
        };
        match value.get("kind").and_then(|k| k.as_str()) {
            Some("access") => match serde_json::from_value::<AccessRecord>(value) {
                Ok(record) => records.push(record),
                // Valid JSON tagged as an access record but missing fields: a
                // writer/reader version skew. Count it rather than guessing.
                Err(_) => skipped += 1,
            },
            Some("access_dropped") => {
                dropped_upstream += value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
            }
            _ => skipped += 1,
        }
    }
    Parsed { records, dropped_upstream, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESS: &str = r#"{"kind":"access","at_ms":1785085129312,"route":"proxy","host":"tide.intern.deepwa7er.net","method":"GET","path":"/theme","status":200,"ms":0,"client_ip":"100.111.100.87","user_agent":"curl/8.7.1"}"#;

    #[test]
    fn parses_access_records() {
        let p = parse(ACCESS);
        assert_eq!(p.records.len(), 1);
        let r = &p.records[0];
        assert_eq!(r.host, "tide.intern.deepwa7er.net");
        assert_eq!(r.status, 200);
        assert_eq!(r.user_agent.as_deref(), Some("curl/8.7.1"));
        assert_eq!(r.query, None, "an absent optional field is None, not an error");
    }

    #[test]
    fn plain_text_journal_lines_are_skipped_not_fatal() {
        // breakwater's own startup/error output shares the journal.
        let text = format!(
            "breakwater: https proxy on 100.98.184.58:443\n{ACCESS}\nbreakwater: upstream error: boom\n"
        );
        let p = parse(&text);
        assert_eq!(p.records.len(), 1, "the one real record still lands");
        assert_eq!(p.skipped, 2);
    }

    #[test]
    fn drop_markers_are_counted_not_ignored() {
        // A gap the warehouse can never recover must stay visible.
        let text = format!("{ACCESS}\n{{\"kind\":\"access_dropped\",\"count\":7}}\n{ACCESS}");
        let p = parse(&text);
        assert_eq!(p.records.len(), 2);
        assert_eq!(p.dropped_upstream, 7);
    }

    #[test]
    fn a_malformed_record_does_not_block_the_rest() {
        let text = format!(
            "{{not json\n{ACCESS}\n{{\"kind\":\"access\",\"at_ms\":1}}\n{ACCESS}"
        );
        let p = parse(&text);
        assert_eq!(p.records.len(), 2, "both good records survive a bad neighbour");
        assert_eq!(p.skipped, 2, "unparseable line + access record missing fields");
    }

    #[test]
    fn unrelated_json_is_not_mistaken_for_a_record() {
        let p = parse(r#"{"level":"info","msg":"something else"}"#);
        assert_eq!(p.records.len(), 0);
        assert_eq!(p.skipped, 1);
    }

    #[test]
    fn empty_input_is_a_clean_no_op() {
        let p = parse("\n\n   \n");
        assert_eq!(p.records.len(), 0);
        assert_eq!(p.skipped, 0);
        assert_eq!(p.dropped_upstream, 0);
    }
}
