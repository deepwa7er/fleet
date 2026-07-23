//! atlas — map and trace the fleet's Rust code.
//!
//! rust-analyzer's SCIP export turns a workspace into symbols + occurrences;
//! atlas derives a symbol graph from it (see `ingest`), stores it in SQLite,
//! and serves a web UI for browsing modules, inspecting symbols, and tracing
//! call flow from any entry point.
//!
//! Like source, atlas is a dev-box service: it needs the working trees and a
//! Rust toolchain, so it runs here (a launchd agent), binds the host's
//! Tailscale IP, and is fronted by breakwater at
//! https://atlas.intern.deepwa7er.net.

mod config;
mod error;
mod ingest;
mod scip;
mod store;
mod symbols;
mod web;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::store::Store;
use crate::web::AppState;

#[derive(Parser)]
#[command(
    name = "atlas",
    about = "Map and trace the deepwa7er fleet's Rust code."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index configured projects into the database and exit.
    Index {
        /// Path to the TOML config (bind address, projects, web dir).
        #[arg(long, default_value = "atlas.toml")]
        config: PathBuf,
        /// Index only this project; absent indexes them all.
        #[arg(long)]
        project: Option<String>,
    },
    /// Run the web service.
    Serve {
        #[arg(long, default_value = "atlas.toml")]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    fleet_common::http::init_tracing("atlas=info,tower_http=info");
    let Cli { command } = Cli::parse();
    let result = match command {
        Command::Index { config, project } => run_index(&config, project.as_deref()),
        Command::Serve { config } => run_serve(&config),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Load config, open the store, register the configured projects.
fn boot(config_path: &Path) -> anyhow::Result<(Config, Arc<AppState>)> {
    let config = Config::load(config_path)?;
    let db_path = config.db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Arc::new(Store::open(&db_path)?);
    for project in &config.projects {
        store.upsert_project(&project.name, &project.path.to_string_lossy())?;
    }
    let state = AppState::new(store, config.rust_analyzer.clone(), config.projects.clone());
    Ok((config, Arc::new(state)))
}

fn run_index(config_path: &Path, only: Option<&str>) -> anyhow::Result<()> {
    let (config, state) = boot(config_path)?;
    let selected: Vec<_> = match only {
        Some(name) => vec![
            config
                .project(name)
                .ok_or_else(|| anyhow::anyhow!("project {name} is not in the config"))?
                .clone(),
        ],
        None => config.projects.clone(),
    };
    for project in selected {
        tracing::info!("indexing {} ({})", project.name, project.path.display());
        let project_id = state.store.project_id(&project.name)?;
        let stats = web::index_project(&state, project_id, &project)?;
        tracing::info!("indexed {}: {stats:?}", project.name);
    }
    Ok(())
}

fn run_serve(config_path: &Path) -> anyhow::Result<()> {
    let (config, state) = boot(config_path)?;
    let addr = SocketAddr::new(config.bind, config.port);
    let app = web::router(state, &config.web_dir);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(
            "atlas listening on http://{addr} (web dir {})",
            config.web_dir.display()
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok(())
    })
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("atlas shutting down");
}
