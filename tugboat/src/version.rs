//! The build identity compiled into the binary by `build.rs`. Both `tugboat
//! version` and the daemon's `GET /health` report this, so a running tugboat can
//! be matched to an exact source build.

use serde::{Deserialize, Serialize};

/// Which build this binary is. Stamped at compile time from git + the build
/// clock; identical for every invocation of the same binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfo {
    /// Crate version (`Cargo.toml`).
    pub version: String,
    /// Short git sha at build time, or `unknown` outside a checkout.
    pub git_sha: String,
    /// Whether the working tree had uncommitted changes when built.
    pub dirty: bool,
    /// Unix seconds at build time.
    pub build_unix: u64,
}

impl BuildInfo {
    /// This binary's build identity.
    pub fn current() -> Self {
        BuildInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_sha: env!("TUGBOAT_GIT_SHA").to_string(),
            dirty: env!("TUGBOAT_GIT_DIRTY") == "true",
            build_unix: env!("TUGBOAT_BUILD_UNIX").parse().unwrap_or(0),
        }
    }

    /// A one-line human description, e.g. `0.1.0 (1a2b3c4d, dirty)`.
    pub fn describe(&self) -> String {
        let dirty = if self.dirty { ", dirty" } else { "" };
        format!("{} ({}{})", self.version, self.git_sha, dirty)
    }
}
