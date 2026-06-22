//! `tugboat fleet hooks` — install the git hooks that auto-refresh the fleet
//! docs on commit.
//!
//! Each hook is a fire-and-forget ping to the `tugboat serve` daemon's
//! `/docs/refresh`. The daemon debounces and rebuilds; the hook never blocks or
//! fails a commit, and a missed ping (daemon down) is caught by the daemon's
//! periodic catch-up — so the hooks are a latency optimization over a system
//! that is already correct without them.
//!
//! Git hooks aren't version-controlled and aren't copied on clone, so the scripts
//! are written to a stable per-machine location (`~/.config/tugboat/hooks`) and
//! each member repo is pointed at it via `core.hooksPath`. The hooks read the
//! daemon URL and token from sibling files so nothing machine-specific is baked
//! into the scripts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::fleet::Fleet;
use crate::git;

/// Options for [`install`].
pub struct InstallOpts {
    /// Explicit serve daemon base URL. When `None`, derived from the tailnet IP.
    pub url: Option<String>,
    /// Port for the derived URL.
    pub port: u16,
    /// Explicit bearer token. When `None`, read from `$TUGBOAT_SERVE_TOKEN`.
    pub token: Option<String>,
}

/// The shared trigger the per-event hooks delegate to.
const NOTIFY_DOCS: &str = r#"#!/bin/sh
# Installed by `tugboat fleet hooks install`. Pings the tugboat serve daemon to
# refresh the fleet docs after a change. Fire-and-forget: a commit must never
# block or fail because the daemon is down — the daemon's periodic catch-up
# rebuilds regardless.
cfg="${XDG_CONFIG_HOME:-$HOME/.config}/tugboat"
[ -r "$cfg/serve-url" ] && [ -r "$cfg/serve-token" ] || exit 0
url=$(cat "$cfg/serve-url")
token=$(cat "$cfg/serve-token")
[ -n "$url" ] && [ -n "$token" ] || exit 0
curl -fsS -m 3 -X POST -H "Authorization: Bearer $token" "$url/docs/refresh" >/dev/null 2>&1 &
exit 0
"#;

/// A per-event hook (post-commit/-merge/-rewrite) that delegates to `notify-docs`
/// in the same directory.
const HOOK_WRAPPER: &str = r#"#!/bin/sh
# Installed by `tugboat fleet hooks install` — delegates to the shared trigger.
exec "$(dirname "$0")/notify-docs"
"#;

/// The git events that change committed state and so should refresh the docs:
/// a commit (incl. amend), a merge (incl. `pull`), and a history rewrite (rebase).
const HOOK_EVENTS: [&str; 3] = ["post-commit", "post-merge", "post-rewrite"];

/// Install the hooks into every fleet member and write the daemon URL/token they
/// read.
pub fn install(fleet: &Fleet, opts: &InstallOpts) -> Result<()> {
    let cfg = config_dir()?;
    let hooks = cfg.join("hooks");
    fs::create_dir_all(&hooks).with_context(|| format!("creating {}", hooks.display()))?;

    write_exec(&hooks.join("notify-docs"), NOTIFY_DOCS)?;
    for event in HOOK_EVENTS {
        write_exec(&hooks.join(event), HOOK_WRAPPER)?;
    }
    println!("Hook scripts → {}", hooks.display());

    let url = match &opts.url {
        Some(url) => url.clone(),
        None => derive_url(opts.port),
    };
    fs::write(cfg.join("serve-url"), format!("{url}\n"))
        .with_context(|| format!("writing {}", cfg.join("serve-url").display()))?;
    println!("  serve-url:   {url}");

    match opts
        .token
        .clone()
        .or_else(|| std::env::var("TUGBOAT_SERVE_TOKEN").ok())
        .filter(|t| !t.is_empty())
    {
        Some(token) => {
            write_secret(&cfg.join("serve-token"), &token)?;
            println!("  serve-token: written ({})", cfg.join("serve-token").display());
        }
        None => eprintln!(
            "  warning: no token (pass --token or set TUGBOAT_SERVE_TOKEN); the hooks can't \
             authenticate until you fill {}",
            cfg.join("serve-token").display()
        ),
    }

    let hooks_path = hooks.to_string_lossy().into_owned();
    println!("\nPointing fleet repos at the hooks:");
    let mut installed = 0;
    for (label, dir) in hooked_repos(fleet) {
        if !dir.join(".git").is_dir() {
            println!("  {label:<14} (no checkout — skipped)");
            continue;
        }
        if git::run(&dir, &["config", "core.hooksPath", &hooks_path])? {
            installed += 1;
            println!("  {label:<14} core.hooksPath set");
        } else {
            eprintln!("  {label:<14} failed to set core.hooksPath");
        }
    }
    println!("\n✓ docs commit-hook installed in {installed} repo(s)");
    println!(
        "  (re-run after `fleet clone`/adding a member; the daemon's catch-up covers any gap)"
    );
    Ok(())
}

/// Remove the hooks from every fleet member (unset `core.hooksPath`). The shared
/// scripts are left in place; re-running `install` re-points members at them.
pub fn uninstall(fleet: &Fleet) -> Result<()> {
    for (label, dir) in hooked_repos(fleet) {
        if !dir.join(".git").is_dir() {
            continue;
        }
        // `--unset` exits non-zero when the key is already absent; that's fine.
        let _ = git::run(&dir, &["config", "--unset", "core.hooksPath"]);
        println!("  {label:<14} core.hooksPath unset");
    }
    println!("\n✓ docs commit-hook removed from fleet repos");
    Ok(())
}

/// The repos whose commits should refresh the docs: every fleet member, plus any
/// `[docs] extra_loc` repos (e.g. the deployer) — these contribute to the docs'
/// line count, so a commit to them should rebuild too. Returned as
/// (label, checkout dir) pairs; extra repos already present as a member are
/// skipped so nothing is hooked twice.
fn hooked_repos(fleet: &Fleet) -> Vec<(String, PathBuf)> {
    let mut repos: Vec<(String, PathBuf)> =
        fleet.members.iter().map(|m| (m.label().to_owned(), fleet.dir(m))).collect();
    if let Some(docs) = &fleet.docs {
        for rel in &docs.extra_loc {
            let dir = fleet.root_dir().join(rel);
            if !repos.iter().any(|(_, d)| d == &dir) {
                repos.push((rel.clone(), dir));
            }
        }
    }
    repos
}

/// Derive the serve daemon URL from the host's tailnet IP (the daemon binds it so
/// the VPS dashboard can reach it; the dev box reaches its own tailnet IP too).
fn derive_url(port: u16) -> String {
    let ip = Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty());
    match ip {
        Some(ip) => format!("http://{ip}:{port}"),
        None => {
            eprintln!(
                "  warning: could not derive the tailnet IP (`tailscale ip -4`); \
                 defaulting to loopback — pass --url if the daemon binds elsewhere"
            );
            format!("http://127.0.0.1:{port}")
        }
    }
}

/// The tugboat config directory, `~/.config/tugboat` (honouring `XDG_CONFIG_HOME`).
fn config_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("no HOME or XDG_CONFIG_HOME to locate the tugboat config dir")?;
    Ok(base.join("tugboat"))
}

/// Write an executable script (mode 0755).
fn write_exec(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    set_mode(path, 0o755)
}

/// Write a secret file (mode 0600), appending a trailing newline.
fn write_secret(path: &Path, content: &str) -> Result<()> {
    fs::write(path, format!("{content}\n")).with_context(|| format!("writing {}", path.display()))?;
    set_mode(path, 0o600)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting mode on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}
