//! tugboat — a small, manifest-driven deployer for personal VPS services.
//!
//! Each service repo carries a `deploy.toml` (and optionally an untracked
//! `deploy.local.toml`). `tugboat deploy` builds the artifact, ships it, swaps
//! it in atomically, restarts the unit, health-checks it, and rolls back if the
//! new build fails to come up — then optionally enrolls the unit in lighthouse.
//!
//! `tugboat fleet …` operates on the whole suite at once (clone, pull, deploy,
//! status), driven by a `fleet.toml` that lists the member repos.

mod agent;
mod deploy;
mod docs;
mod fleet;
mod git;
mod hooks;
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
    /// Deploy a per-user daemon (launchd / systemd-user) to dev machines.
    Agent(AgentArgs),
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
struct AgentArgs {
    #[command(subcommand)]
    action: AgentAction,
}

#[derive(Subcommand)]
enum AgentAction {
    /// Build and install the agent on each target machine.
    Deploy(AgentDeployArgs),
}

#[derive(Parser)]
struct AgentDeployArgs {
    /// Path to the agent manifest.
    #[arg(long, default_value = "agent.toml")]
    manifest: PathBuf,
    /// Restrict to a comma-separated set of target names.
    #[arg(long)]
    only: Option<String>,
    /// Print the plan and exit without building or installing.
    #[arg(long)]
    dry_run: bool,
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
    /// Generate the fleet documentation site (fleet.json + per-repo rustdoc).
    Docs(FleetDocsArgs),
    /// Install/remove the git hooks that auto-refresh the docs on commit.
    Hooks(FleetHooksArgs),
}

#[derive(Parser)]
struct FleetHooksArgs {
    #[command(subcommand)]
    action: FleetHooksAction,
}

#[derive(Subcommand)]
enum FleetHooksAction {
    /// Install the hooks in every member and write the daemon URL/token they use.
    Install(FleetHooksInstallArgs),
    /// Remove the hooks from every member (unset `core.hooksPath`).
    Uninstall,
}

#[derive(Parser)]
struct FleetHooksInstallArgs {
    /// serve daemon base URL the hooks POST to. Default: derived from `tailscale ip -4`.
    #[arg(long)]
    url: Option<String>,
    /// Port for the derived URL.
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Bearer token the hooks present. Default: `$TUGBOAT_SERVE_TOKEN`.
    #[arg(long)]
    token: Option<String>,
}

#[derive(Parser)]
struct FleetDocsArgs {
    /// Assemble the site into this directory and don't ship. Omit to ship to the
    /// host in the fleet's `[docs]` config.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Skip building the frontend and reuse its existing `dist`.
    #[arg(long)]
    skip_build: bool,
    /// Skip the slow `cargo doc` pass and emit only fleet.json.
    #[arg(long)]
    skip_rustdoc: bool,
    /// Restrict the rustdoc pass to a comma-separated set of member names
    /// (fleet.json still covers the whole fleet).
    #[arg(long)]
    only: Option<String>,
    /// Print the plan and exit without building or shipping.
    #[arg(long)]
    dry_run: bool,
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
        Command::Agent(args) => match args.action {
            AgentAction::Deploy(d) => agent::deploy(&d.manifest, d.only.as_deref(), d.dry_run),
        },
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
    deploy::run(&manifest, &project_dir, args.skip_build, args.dry_run, &deploy::StdoutSink)?;

    // A real deploy also enrolls the service in the fleet roster if it isn't
    // already there, so it shows up in `fleet` ops and lighthouse's deploy view.
    // Best-effort: never fail a successful deploy over registration.
    if !args.dry_run {
        match fleet::ensure_member(&project_dir, &manifest.name) {
            Ok(fleet::Registration::Registered { fleet_path, .. }) => println!(
                "\n==> registered {} in the fleet ({})\n    commit & push tugboat to persist it.",
                manifest.name,
                fleet_path.display()
            ),
            Ok(fleet::Registration::AlreadyMember) => {}
            Ok(fleet::Registration::Skipped(why)) => {
                eprintln!("\n   note: did not auto-register {} in the fleet — {why}", manifest.name)
            }
            Err(err) => {
                eprintln!("\n   note: fleet registration check failed: {err:#}")
            }
        }
    }
    Ok(())
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
        FleetAction::Docs(d) => {
            let only = d.only.map(|s| {
                s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
            });
            docs::generate(
                &fleet,
                &docs::Options {
                    out: d.out,
                    skip_build: d.skip_build,
                    skip_rustdoc: d.skip_rustdoc,
                    only,
                    dry_run: d.dry_run,
                },
            )
        }
        FleetAction::Hooks(h) => match h.action {
            FleetHooksAction::Install(a) => hooks::install(
                &fleet,
                &hooks::InstallOpts { url: a.url, port: a.port, token: a.token },
            ),
            FleetHooksAction::Uninstall => hooks::uninstall(&fleet),
        },
    }
}
