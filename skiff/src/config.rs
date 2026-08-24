//! Where skiffd's files and ports come from.
//!
//! Every path has a working default and a flag; there is no configuration
//! file, because a single-host service with four paths does not need one.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use crate::ingest::pi;

/// The agent desk: harness sessions and fleet changes as live queries.
#[derive(Debug, Parser)]
#[command(name = "skiffd", version)]
pub struct Config {
    /// Address to listen on. Defaults to the tailnet-facing port beside the
    /// stack skiffd replaces, so both can run at once until the cutover
    /// (DW-004 §13).
    #[arg(long, env = "SKIFF_ADDR", default_value = "0.0.0.0:8121")]
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
}

/// `$XDG_STATE_HOME/skiff`, falling back to `~/.local/state/skiff`. State, not
/// cache: the store is cheap to rebuild but rebuilding it on every login would
/// make every cold start slow for no reason.
fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("skiff");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    home.join(".local/state/skiff")
}
