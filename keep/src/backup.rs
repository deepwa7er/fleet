//! keep backs itself up: every minute each database is snapshotted with
//! `VACUUM INTO` (a consistent plain-SQLite file — see
//! [`Registry::snapshot_all`]) and the snapshot dir is handed to restic → R2.
//!
//! Two retention tiers, matching DW-005: minute snapshots are the recovery
//! point (sixty seconds), nightly snapshots tagged `keep-nightly` are the
//! long memory (`--keep-daily 7 --keep-weekly 4 --keep-monthly 6`). The
//! minute tier is forgotten past 48h — it is a recovery point, not an
//! archive, and an ever-growing minute tier would turn every prune into a
//! full-repo walk.
//!
//! restic reads its repository, password, and R2 credentials from the
//! environment (the unit's `EnvironmentFile=/etc/keep/restic.env`, same
//! contract as fleet-backup). With no `RESTIC_REPOSITORY` set, snapshots
//! still accumulate locally and restic is skipped with a warning — keep
//! serves either way, but it tells you the off-box half is missing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::store::Registry;

const MINUTE_TAG: &str = "keep-minute";
const NIGHTLY_TAG: &str = "keep-nightly";

pub struct BackupConfig {
    pub snapshot_dir: PathBuf,
    /// Seconds between snapshots. 60 in production; tests pass 1.
    pub interval_secs: u64,
}

pub async fn snapshot_loop(
    registry: Arc<Registry>,
    config: BackupConfig,
) -> anyhow::Result<()> {
    if std::env::var("RESTIC_REPOSITORY").is_err() {
        tracing::warn!(
            "RESTIC_REPOSITORY is unset — snapshots accumulate in {} with no off-box copy",
            config.snapshot_dir.display()
        );
    }
    let mut last_nightly: Option<std::time::Instant> = None;
    loop {
        if let Err(e) = snapshot_once(&registry, &config).await {
            // Loud, not fatal: a failed snapshot must never take the store
            // down with it, and the next tick retries in a minute.
            tracing::error!("snapshot failed: {e:?}");
        } else if nightly_due(last_nightly) {
            if let Err(e) = nightly_pass(&config.snapshot_dir).await {
                tracing::error!("nightly backup pass failed: {e:?}");
            } else {
                last_nightly = Some(std::time::Instant::now());
            }
        }
        tokio::time::sleep(Duration::from_secs(config.interval_secs)).await;
    }
}

fn nightly_due(last: Option<std::time::Instant>) -> bool {
    last.is_none_or(|t| t.elapsed() >= Duration::from_secs(24 * 3600))
}

async fn snapshot_once(registry: &Registry, config: &BackupConfig) -> anyhow::Result<()> {
    let files = registry.snapshot_all(&config.snapshot_dir).await?;
    tracing::info!("snapshotted {} database(s)", files.len());
    restic_backup(MINUTE_TAG, &config.snapshot_dir).await
}

async fn nightly_pass(snapshot_dir: &std::path::Path) -> anyhow::Result<()> {
    restic_backup(NIGHTLY_TAG, snapshot_dir).await?;
    // Minute tier: recovery point, not archive.
    restic_forget(Some(MINUTE_TAG), &["--keep-within", "48h"]).await?;
    // Nightly tier: 7 daily, 4 weekly, 6 monthly.
    restic_forget(
        Some(NIGHTLY_TAG),
        &["--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "6"],
    )
    .await
}

fn restic_available() -> bool {
    std::env::var("RESTIC_REPOSITORY").is_ok()
}

async fn restic_backup(tag: &str, dir: &std::path::Path) -> anyhow::Result<()> {
    if !restic_available() {
        tracing::warn!("skipping restic backup (no RESTIC_REPOSITORY)");
        return Ok(());
    }
    // The snapshot dir comes from keep's own config, not the request path —
    // no client input reaches this command line.
    let dir = dir.to_string_lossy().into_owned();
    run_restic(&["backup", "--tag", tag, &dir]).await
}

async fn restic_forget(tag: Option<&str>, policy: &[&str]) -> anyhow::Result<()> {
    if !restic_available() {
        return Ok(());
    }
    let mut args = vec!["forget", "--prune"];
    if let Some(tag) = tag {
        args.push("--tag");
        args.push(tag);
    }
    args.extend(policy);
    run_restic(&args).await
}

async fn run_restic(args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new("restic").args(args).output().await?;
    if !out.status.success() {
        anyhow::bail!(
            "restic {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    tracing::info!("restic {} ok", args.join(" "));
    Ok(())
}
