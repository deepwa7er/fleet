//! tide — fleet-wide settings for the deepwa7er VPS fleet. Today it holds one
//! thing: the shared dark/light **theme** that every fleet web UI honors. It's
//! the single source of truth (persisted on the VPS), flipped from anywhere via
//! ferry (`b dark` / `b light` redirect to `/set`), and read by each UI over a
//! CORS-open `/theme`.
//!
//! `bind` is loopback; breakwater is the tailnet front door (no auth — the
//! tailnet is the boundary, same model as the rest of the fleet).

mod config;
mod store;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::store::Store;

#[derive(Parser)]
#[command(name = "tide", about = "Fleet-wide settings (shared theme) for the deepwa7er fleet.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web service.
    Serve {
        /// Path to the TOML config (bind address, port, data dir).
        #[arg(long, default_value = "tide.toml")]
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

    let store = Arc::new(Store::open(&config.data_dir));

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: building async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async move {
        let addr = SocketAddr::new(config.bind, config.port);
        let app = web::router(Arc::clone(&store));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("binding {addr}: {e}"))?;
        eprintln!("tide listening on {addr} (theme: {})", store.get());
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
    eprintln!("tide shutting down");
}
