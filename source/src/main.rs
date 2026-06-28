//! source — browse and search the deepwa7er fleet's source code from one page.
//!
//! The fleet's canonical working trees live only on the dev box (this machine),
//! at the paths `fleet.toml` lists — the VPS holds built binaries, not source.
//! So this service runs on the dev box (a launchd agent, like `tugboat serve`),
//! reads those working trees directly, and is fronted by breakwater at
//! https://source.internal.deepwa7er.com.
//!
//! Because breakwater runs on the VPS and must reach this service across the
//! tailnet, `bind` is the host's Tailscale IP (not loopback). The tailnet is the
//! security boundary, the same model as the rest of the fleet — and v1 is
//! read-only (browse + search), so any tailnet device may read the source. When
//! editing lands, write access deserves a second look (see README).
//!
//! Everything served is **tracked** source only — file listings come from
//! `git ls-files` and search runs through ripgrep, so `.gitignore`d secrets and
//! build output are excluded by construction.

mod config;
mod fleet;
mod repo;
mod search;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::fleet::Fleet;

#[derive(Parser)]
#[command(name = "source", about = "Browse and search the deepwa7er fleet's source code.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web service.
    Serve {
        /// Path to the TOML config (bind address, port, fleet manifest, web dir).
        #[arg(long, default_value = "source.toml")]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    let Cli { command } = Cli::parse();
    match command {
        Command::Serve { config } => serve(&config),
    }
}

fn serve(config_path: &std::path::Path) -> ExitCode {
    let config = match config::Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    // Load the fleet once at startup. The working set rarely changes; a restart
    // (the launchd agent's KeepAlive makes that cheap) picks up edits to
    // `fleet.toml`. Each request re-reads the trees themselves, so source edits
    // are always live.
    let fleet = match Fleet::load(&config.fleet) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: loading fleet manifest {}: {e:#}", config.fleet.display());
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "source: {} repos from {} (root {})",
        fleet.repos().len(),
        config.fleet.display(),
        fleet.root().display()
    );

    let state = Arc::new(web::AppState { fleet });

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: building async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result: anyhow::Result<()> = runtime.block_on(async move {
        let addr = SocketAddr::new(config.bind, config.port);
        let app = web::router(state, &config.web_dir);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("source listening on http://{addr} (web dir {})", config.web_dir.display());
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
        Ok(())
    });

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("source shutting down");
}
