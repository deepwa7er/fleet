mod core;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use fleet_common::util::{default_db_path, env_or};

use core::Store;

#[derive(Parser)]
#[command(
    name = "regatta",
    about = "Sequence-voting party game: propose a ten-step course, the crew votes"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server: web view + JSON API. (The default with no subcommand.)
    Serve,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Only one subcommand today; serving is also the default.
    match cli.command {
        None | Some(Command::Serve) => {
            if let Err(e) = run_serve().await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    fleet_common::http::init_tracing("regatta=info,tower_http=info");

    let db_path = std::env::var("REGATTA_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path("regatta", "regatta.db"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Arc::new(Store::open(&db_path)?);
    tracing::info!("database at {}", db_path.display());

    let config = server::ServerConfig {
        addr: env_or("REGATTA_ADDR", "127.0.0.1:8096").parse::<SocketAddr>()?,
        web_dir: PathBuf::from(env_or("REGATTA_WEB_DIR", "web/dist")),
    };

    server::run(store, config).await?;
    Ok(())
}
