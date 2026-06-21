//! tugboat — a small, manifest-driven deployer for personal VPS services.
//!
//! Each service repo carries a `deploy.toml` (and optionally an untracked
//! `deploy.local.toml`). `tugboat deploy` builds the artifact, ships it, swaps
//! it in atomically, restarts the unit, health-checks it, and rolls back if the
//! new build fails to come up — then optionally enrolls the unit in lighthouse.
//!
//! `tugboat fleet …` operates on the whole suite at once (clone, pull, deploy,
//! status), driven by a `fleet.toml` that lists the member repos.

mod deploy;
mod fleet;
mod git;
mod manifest;
mod selfdeploy;
mod serve;
mod version;

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::version::BuildInfo;

#[derive(Parser)]
#[command(
    name = "tugboat",
    about = "Manifest-driven deployer for personal VPS services",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Deploy a single service from its `deploy.toml`.
    Deploy(DeployArgs),
    /// Operate on the whole fleet (driven by `fleet.toml`).
    Fleet(FleetArgs),
    /// Run the HTTP deploy daemon (drives the fleet's deploys from another host).
    Serve(ServeArgs),
    /// Rebuild tugboat and roll it into the local `serve` launchd agent.
    SelfDeploy(SelfDeployArgs),
    /// Print this binary's build identity (git sha, build time).
    Version(VersionArgs),
}

#[derive(Parser)]
struct DeployArgs {
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

#[derive(Parser)]
struct ServeArgs {
    /// Address to bind. Use this machine's tailnet IP so the dashboard on the
    /// VPS can reach it; the default keeps it loopback-only until you opt in.
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,
    /// Port to listen on.
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Path to the fleet manifest (defaults like `tugboat fleet`).
    #[arg(long)]
    manifest: Option<PathBuf>,
}

#[derive(Parser)]
struct SelfDeployArgs {
    /// The tugboat source checkout to build.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Where to install the binary the launchd agent runs.
    #[arg(long)]
    install_path: Option<PathBuf>,
    /// launchd agent label to restart.
    #[arg(long, default_value_t = selfdeploy::DEFAULT_LABEL.to_string())]
    label: String,
    /// Full `/health` URL to poll (default derived from `tailscale ip -4`).
    #[arg(long)]
    health_url: Option<String>,
    /// Port for the derived health URL (when `--health-url` isn't given).
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// How long to wait for the daemon to come back, in seconds.
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
    /// Reuse the existing release build instead of rebuilding.
    #[arg(long)]
    skip_build: bool,
    /// Print the plan and exit without changing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Parser)]
struct VersionArgs {
    /// Emit the build identity as JSON instead of a human-readable line.
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct FleetArgs {
    /// Path to the fleet manifest. Defaults to TUGBOAT_FLEET, else the nearest
    /// `fleet.toml` searching upward from the current directory.
    #[arg(long)]
    manifest: Option<PathBuf>,
    #[command(subcommand)]
    action: FleetAction,
}

#[derive(Subcommand)]
enum FleetAction {
    /// List the configured members.
    List,
    /// Clone any members not yet checked out.
    Clone,
    /// Fast-forward-only pull every clean member checkout.
    Pull,
    /// Show a git summary per member.
    Status,
    /// Deploy each deployable member in listed order.
    Deploy(FleetDeployArgs),
}

#[derive(Parser)]
struct FleetDeployArgs {
    /// Restrict to a comma-separated set of member names.
    #[arg(long)]
    only: Option<String>,
    /// Reuse existing build artifacts instead of rebuilding.
    #[arg(long)]
    skip_build: bool,
    /// Print each member's plan and exit without changing anything.
    #[arg(long)]
    dry_run: bool,
    /// Deploy remaining members even if one fails (default: stop on first failure).
    #[arg(long)]
    continue_on_error: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Deploy(args) => run_deploy(args),
        Command::Fleet(args) => run_fleet(args),
        Command::Serve(args) => serve::run(serve::ServeArgs {
            bind: args.bind,
            port: args.port,
            manifest: args.manifest,
        }),
        Command::SelfDeploy(args) => selfdeploy::run(selfdeploy::SelfDeployArgs {
            repo: args.repo,
            install_path: args.install_path,
            label: args.label,
            health_url: args.health_url,
            port: args.port,
            timeout_secs: args.timeout_secs,
            skip_build: args.skip_build,
            dry_run: args.dry_run,
        }),
        Command::Version(args) => {
            let info = BuildInfo::current();
            if args.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("tugboat {}", info.describe());
            }
            Ok(())
        }
    }
}

fn run_deploy(args: DeployArgs) -> Result<()> {
    let manifest_path = args
        .manifest
        .canonicalize()
        .with_context(|| format!("manifest not found: {}", args.manifest.display()))?;
    let project_dir = manifest_path
        .parent()
        .context("manifest has no parent directory")?
        .to_path_buf();

    let manifest = manifest::load(&manifest_path, args.host.as_deref())?;
    deploy::run(&manifest, &project_dir, args.skip_build, args.dry_run, &deploy::StdoutSink)
}

fn run_fleet(args: FleetArgs) -> Result<()> {
    let manifest_path = fleet::resolve_manifest(args.manifest.as_deref())?;
    let fleet = fleet::load(&manifest_path)?;
    match args.action {
        FleetAction::List => {
            fleet::list(&fleet);
            Ok(())
        }
        FleetAction::Clone => fleet::clone(&fleet),
        FleetAction::Pull => fleet::pull(&fleet),
        FleetAction::Status => fleet::status(&fleet),
        FleetAction::Deploy(d) => fleet::deploy(
            &fleet,
            d.only.as_deref(),
            d.skip_build,
            d.dry_run,
            d.continue_on_error,
        ),
    }
}
