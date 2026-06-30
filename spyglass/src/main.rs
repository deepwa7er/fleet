//! spyglass — federated search across the deepwa7er fleet. One box that fans a
//! query out to the fleet's search services — `source` (code) and `lagoon`
//! (notes) today — and shows the results grouped by source.
//!
//! `bind` is loopback; breakwater is the tailnet front door (no auth — the
//! tailnet is the boundary, same model as the rest of the fleet). spyglass holds
//! no state of its own: every result is fetched live from an upstream.

mod config;
mod search;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::search::Engine;

#[derive(Parser)]
#[command(name = "spyglass", about = "Federated fleet search for the deepwa7er fleet.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web service.
    Serve {
        /// Path to the TOML config (bind address, port, sources).
        #[arg(long, default_value = "spyglass.toml")]
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
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let addr = SocketAddr::new(config.bind, config.port);
    let source_count = config.sources.len();

    let engine = match Engine::new(config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("error: building search engine: {e}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: building async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async move {
        let app = web::router(engine);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("binding {addr}: {e}"))?;
        eprintln!("spyglass listening on {addr} ({source_count} sources)");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| format!("server error: {e}"))
    });

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("spyglass shutting down");
}
