//! Stamp the binary with its git identity and build time so a running tugboat
//! can report exactly which build it is (`tugboat version`, `GET /health`).
//!
//! Everything is best-effort: building outside a git checkout (e.g. from a
//! tarball) yields `unknown`/`false` rather than failing the build. The sha is
//! re-read when `HEAD` moves or the index changes; that covers committed work,
//! which is what the human-facing sha is for. The *safety* signal that a
//! self-deploy actually restarted the daemon is the daemon's process start time
//! (see `serve`), not this stamp, so a slightly stale sha here is cosmetic.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let sha = git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    let build_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    println!("cargo:rustc-env=TUGBOAT_GIT_SHA={sha}");
    println!("cargo:rustc-env=TUGBOAT_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=TUGBOAT_BUILD_UNIX={build_unix}");

    // Re-stamp when the commit or the index changes.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}

/// Run a git command at the crate root, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
