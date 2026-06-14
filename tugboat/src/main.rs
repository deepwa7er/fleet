//! tugboat — a small, manifest-driven deployer for personal VPS services.
//!
//! Each service repo carries a `deploy.toml` (and optionally an untracked
//! `deploy.local.toml`). tugboat builds the artifact, ships it, swaps it in
//! atomically, restarts the unit, health-checks it, and rolls back if the new
//! build fails to come up — then optionally enrolls the unit in lighthouse.

mod deploy;
mod manifest;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "tugboat",
    about = "Manifest-driven deployer for personal VPS services",
    version
)]
struct Cli {
    /// Path to the deploy manifest.
    #[arg(long, default_value = "deploy.toml")]
    manifest: PathBuf,

    /// Override the SSH host (alias) from the manifest.
    #[arg(long)]
    host: Option<String>,

    /// Reuse existing build artifacts instead of rebuilding.
    #[arg(long)]
    skip_build: bool,

    /// Print the plan and exit without changing anything.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let manifest_path = cli
        .manifest
        .canonicalize()
        .with_context(|| format!("manifest not found: {}", cli.manifest.display()))?;
    let project_dir = manifest_path
        .parent()
        .context("manifest has no parent directory")?
        .to_path_buf();

    let manifest = manifest::load(&manifest_path, cli.host.as_deref())?;
    deploy::run(&manifest, &project_dir, cli.skip_build, cli.dry_run)
}
