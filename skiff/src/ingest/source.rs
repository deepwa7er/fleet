//! The seam between a harness's own file format and the domain model (DW-004 §4).
//!
//! This is the **file-tailed** contract: one implementation per harness that
//! owns session files (pi, muse). An adapter's whole job is to turn its native
//! format into domain records; it never touches HTTP, SQL, or views, and
//! nothing above it knows that pi writes a `parentId` tree while muse writes
//! a flat event log.
//!
//! OpenCode is deliberately not a `Source`: it owns its sessions behind
//! `opencode serve`, where there is no directory to tail, so
//! [`super::opencode::OpencodeIngest`] polls HTTP/SSE instead. The two
//! pipelines share the services in [`super::loop_services`], not this trait.
//!
//! ## Source state
//!
//! Both adapters need to remember something between incremental reads, and it
//! turns out to be the same concept wearing two hats:
//!
//! - pi's session **header** is line 1 of the file, so a read resuming from a
//!   byte watermark never sees it again.
//! - muse's **model** is established cumulatively by records scattered through
//!   the log, so a batch read from the middle does not know it.
//!
//! Rather than special-casing either, an adapter gets an opaque JSON value it
//! may read at the start of a read and replace at the end. The store persists
//! it and has no opinion about its shape.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::model::{Entry, Harness, SessionKey, SessionSummary};

/// One session file a source can see.
pub struct Discovered {
    pub key: SessionKey,
    pub path: PathBuf,
}

/// What a read of one batch of lines produced.
#[derive(Default)]
pub struct ParsedBatch {
    pub entries: Vec<Entry>,
    /// What to remember for the next read. `None` leaves the stored value
    /// alone — the honest answer when this batch learned nothing new.
    pub state: Option<Value>,
}

impl ParsedBatch {
    /// Carry the stored state forward when this batch produced none.
    pub fn keeping(mut self, previous: Option<&Value>) -> Self {
        if self.state.is_none() {
            self.state = previous.cloned();
        }
        self
    }
}

pub trait Source: Send + Sync {
    /// Names this source in errors and in the health readout.
    fn name(&self) -> &'static str;

    /// The harness whose sessions this source owns.
    ///
    /// Declared rather than inferred from what a scan found: a source that
    /// legitimately has *no* sessions any more must still be able to forget
    /// the ones it had, and "discovered nothing" cannot distinguish that from
    /// "discovered nothing because something broke".
    fn harness(&self) -> Harness;

    /// The directory to scan and watch. A source whose root does not exist is
    /// degraded, not fatal (DW-004 §4).
    fn root(&self) -> &Path;

    /// Every session file this source can currently see.
    ///
    /// Blocking: it walks the filesystem.
    fn discover(&self) -> Result<Vec<Discovered>>;

    /// Parse newly-read lines into entries.
    ///
    /// `first_line` is the file index of `lines[0]`, which is what gives an
    /// entry a `seq` that is stable across re-reads. `state` is what the
    /// previous read stored, or `None` after a restart.
    fn parse(&self, lines: &[String], first_line: i64, state: Option<&Value>) -> ParsedBatch;

    /// Derive the session's summary from every entry it has.
    ///
    /// `None` means "these are not a session" — a `.jsonl` file that is
    /// something else, or a session directory the harness created but has not
    /// written to yet. Such a file must not appear in the list.
    fn summarize(
        &self,
        key: &SessionKey,
        state: Option<&Value>,
        entries: &[Entry],
    ) -> Option<SessionSummary>;
}
