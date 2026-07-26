//! depot — the fleet's data warehouse.
//!
//! The fleet's services each hold a snapshot of *now*: when a drydock ticket
//! closes, the fact that it was open three days is gone. depot is the historian
//! for the facts that would otherwise be overwritten or pruned.
//!
//! It runs on the VPS rather than the dev box deliberately: the dev box sleeps,
//! and a warehouse with holes in it is not a warehouse. That also puts it on the
//! same host as breakwater's journal, so access-log ingest is a local read
//! instead of a delivery that can fail.
//!
//! Two sources today, one pull and one push:
//!
//! - **breakwater's access log** — pulled from journald on an interval
//!   ([`ingest`]). Which services actually get used; nothing else records it.
//! - **tugboat's deploy events** — pushed to `/api/events/deploy`, because
//!   tugboat runs on the dev box and cannot be polled.

mod ingest;
mod schema;
mod server;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use fleet_common::util::{default_db_path, env_or};

use store::Store;

#[derive(Parser)]
#[command(name = "depot", about = "The fleet's data warehouse")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server and the ingest loop. (The default with no subcommand.)
    Serve,
    /// Run one ingest pass and exit, reporting what it found. For checking a
    /// deployment without waiting for the interval, and for backfilling by hand
    /// after deleting the cursor file.
    Ingest,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        None | Some(Command::Serve) => run_serve().await,
        Some(Command::Ingest) => run_ingest_once().await,
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Settings shared by both subcommands.
struct Settings {
    db_path: PathBuf,
    ingest: ingest::Config,
}

fn settings() -> Result<Settings, Box<dyn std::error::Error>> {
    let db_path = std::env::var("DEPOT_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path("depot", "depot.db"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cursor_file = std::env::var("DEPOT_CURSOR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path("depot", "breakwater.cursor"));
    Ok(Settings {
        db_path,
        ingest: ingest::Config {
            unit: env_or("DEPOT_ACCESS_UNIT", "breakwater"),
            cursor_file,
            interval: Duration::from_secs(
                env_or("DEPOT_INGEST_INTERVAL_SECS", "60").parse().unwrap_or(60),
            ),
        },
    })
}

async fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    fleet_common::http::init_tracing("depot=info,tower_http=info");
    let settings = settings()?;
    let store = Arc::new(Store::open(&settings.db_path)?);
    tracing::info!("database at {}", settings.db_path.display());

    // Ingest runs alongside the server rather than on a timer unit: it needs the
    // same store, and a pass is a cheap `journalctl` read.
    let ingest_store = store.clone();
    tokio::spawn(async move { ingest::run(ingest_store, settings.ingest).await });

    let config = server::Config {
        addr: env_or("DEPOT_ADDR", "127.0.0.1:8100").parse::<SocketAddr>()?,
        web_dir: PathBuf::from(env_or("DEPOT_WEB_DIR", "web/dist")),
    };
    server::run(store, config).await?;
    Ok(())
}

async fn run_ingest_once() -> Result<(), Box<dyn std::error::Error>> {
    let settings = settings()?;
    let store = Store::open(&settings.db_path)?;
    let pass = ingest::once(&store, &settings.ingest).await?;
    println!(
        "seen {} · stored {} · skipped {} · dropped upstream {}",
        pass.seen, pass.stored, pass.skipped, pass.dropped_upstream
    );
    if pass.dropped_upstream > 0 {
        println!(
            "warning: breakwater dropped {} record(s) under load — permanently absent",
            pass.dropped_upstream
        );
    }
    Ok(())
}
