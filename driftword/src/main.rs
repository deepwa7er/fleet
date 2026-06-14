//! driftword — generates pronounceable fake words. One binary, two modes:
//!   * `driftword serve --config <toml>`  — the web UI + JSON API (fleet service)
//!   * `driftword gen [flags]`            — the CLI (replaces wordgen.py)
//!
//! The word corpus and all web assets are embedded, so the binary is the whole
//! deploy. On the VPS it binds the Tailscale IP (tailnet-only, no auth).

mod cli;
mod config;
mod generator;
mod web;
mod wordlist;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::generator::Engine;

#[derive(Parser)]
#[command(name = "driftword", about = "Generate pronounceable fake words (web service + CLI).")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web service.
    Serve {
        /// Path to the TOML config (bind address + port).
        #[arg(long, default_value = "driftword.toml")]
        config: PathBuf,
    },
    /// Generate words on the command line.
    Gen(cli::GenArgs),
}

fn main() -> ExitCode {
    let parsed = Cli::parse();
    let engine = Arc::new(Engine::new(wordlist::load()));

    match parsed.command {
        Command::Gen(args) => match cli::run(&engine, &args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Serve { config } => serve(engine, &config),
    }
}

/// Load config and run the HTTP server until shutdown.
fn serve(engine: Arc<Engine>, config_path: &std::path::Path) -> ExitCode {
    let config = match config::Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
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
        let addr = SocketAddr::new(config.bind, config.port);
        let app = web::router(Arc::clone(&engine));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("binding {addr}: {e}"))?;
        eprintln!("driftword listening on {addr} ({} words embedded)", engine.corpus_len());
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
    eprintln!("driftword shutting down");
}
