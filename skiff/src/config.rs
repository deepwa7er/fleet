//! Where skiffd's files and ports come from.
//!
//! Every path has a working default and a flag; there is no configuration
//! file, because a single-host service with four paths does not need one.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use crate::ingest::loop_services::home;
use crate::ingest::{muse, pi};

/// The agent desk: harness sessions and fleet changes as live queries.
#[derive(Debug, Parser)]
#[command(name = "skiffd", version)]
pub struct Config {
    /// Address to listen on. Development defaults to loopback; the production
    /// wrapper binds the desktop's tailnet IP explicitly. Skiff has no app-level
    /// authentication, so an implicit LAN bind would be unsafe.
    #[arg(long, env = "SKIFF_ADDR", default_value = "127.0.0.1:8120")]
    pub addr: SocketAddr,

    /// The derived read model. Safe to delete at any time: it is rebuilt on
    /// the next scan.
    #[arg(long, env = "SKIFF_STORE")]
    pub store: Option<PathBuf>,

    /// The built client bundle.
    #[arg(long, env = "SKIFF_WEB_DIST")]
    pub web_dist: Option<PathBuf>,

    /// pi's session directory. Defaults to pi's own resolution order.
    #[arg(long, env = "SKIFF_PI_SESSION_DIR")]
    pub pi_session_dir: Option<PathBuf>,

    /// The pi binary. Resolved on PATH by default.
    #[arg(long, env = "SKIFF_PI_BINARY", default_value = "pi")]
    pub pi_binary: PathBuf,

    /// muse's session directory. Defaults to muse's own XDG resolution.
    #[arg(long, env = "SKIFF_MUSE_SESSION_DIR")]
    pub muse_session_dir: Option<PathBuf>,

    /// The muse binary. Resolved on PATH and common home-relative locations.
    #[arg(long, env = "SKIFF_MUSE_BINARY", default_value = "muse")]
    pub muse_binary: PathBuf,

    /// Loopback URL of the sibling `opencode serve` process.
    #[arg(long, env = "SKIFF_OPENCODE_URL", default_value = "http://127.0.0.1:4130")]
    pub opencode_url: String,
}

impl Config {
    pub fn store_path(&self) -> PathBuf {
        self.store.clone().unwrap_or_else(|| state_dir().join("read-model.sqlite3"))
    }

    pub fn web_dist_path(&self) -> PathBuf {
        self.web_dist
            .clone()
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/dist"))
    }

    pub fn pi_dir(&self) -> PathBuf {
        self.pi_session_dir.clone().unwrap_or_else(pi::default_session_dir)
    }

    pub fn pi_binary(&self) -> PathBuf {
        self.pi_binary.clone()
    }

    pub fn muse_dir(&self) -> PathBuf {
        self.muse_session_dir.clone().unwrap_or_else(muse::default_session_dir)
    }

    pub fn muse_binary(&self) -> PathBuf {
        self.muse_binary.clone()
    }

    /// Every source skiffd ingests from.
    ///
    /// A source whose directory does not exist is still listed: it degrades to
    /// a named error the client shows, rather than being silently absent —
    /// which would be indistinguishable from a harness with no sessions.
    pub fn sources(&self) -> Vec<Box<dyn crate::ingest::source::Source>> {
        vec![
            Box::new(pi::Pi::new(self.pi_dir())),
            Box::new(muse::Muse::new(self.muse_dir())),
        ]
    }
}

/// `$XDG_STATE_HOME/skiff`, falling back to `~/.local/state/skiff`. State, not
/// cache: the store is cheap to rebuild but rebuilding it on every login would
/// make every cold start slow for no reason.
fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("skiff");
    }
    home().join(".local/state/skiff")
}
