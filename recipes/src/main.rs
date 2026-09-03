mod core;
mod import;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use fleet_common::keep::Client;
use fleet_common::util::env_or;

use core::Store;

#[derive(Parser)]
#[command(name = "recipes", about = "Recipe book: create, view, and edit recipes")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server: web view + JSON API. (The default with no subcommand.)
    Serve,
    /// One-time import from a local SQLite file into keep. Refuses a
    /// non-empty keep database — imports run once, against an empty store.
    Import {
        /// The SQLite file to read (opened read-only; never modified).
        #[arg(long)]
        from: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Serving is the default; import is the one deliberate exception.
    let result = match cli.command {
        None | Some(Command::Serve) => run_serve().await,
        Some(Command::Import { from }) => run_import(from).await,
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// The keep client from the environment: `RECIPES_KEEP_URL` plus the
/// database's Bearer [REDACTED] either directly (`RECIPES_KEEP_TOKEN`) or via a file
/// (`RECIPES_KEEP_TOKEN_FILE`, the production shape — the unit reads
/// `/etc/recipes/keep-token`, installed by `deploy/provision.sh`, so the
/// secret never sits in an `Environment=` line readable via systemctl).
fn keep_client() -> Result<Client, Box<dyn std::error::Error>> {
    let base = env_or("RECIPES_KEEP_URL", "http://100.73.64.99:8106");
    let token = match std::env::var("RECIPES_KEEP_TOKEN_FILE") {
        Ok(path) => std::fs::read_to_string(&path)
            .map_err(|e| format!("reading token file {path:?}: {e}"))?,
        Err(_) => std::env::var("RECIPES_KEEP_TOKEN")
            .map_err(|_| "set RECIPES_KEEP_TOKEN or RECIPES_KEEP_TOKEN_FILE")?,
    };
    Ok(Client::new(&base, "recipes", token.trim()))
}

async fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    fleet_common::http::init_tracing("recipes=info,tower_http=info");

    let store = Arc::new(Store::open(keep_client()?).await?);
    tracing::info!("recipes serving from keep");

    let config = server::ServerConfig {
        addr: env_or("RECIPES_ADDR", "127.0.0.1:8097").parse::<SocketAddr>()?,
        web_dir: PathBuf::from(env_or("RECIPES_WEB_DIR", "web/dist")),
    };

    server::run(store, config).await?;
    Ok(())
}

async fn run_import(from: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    fleet_common::http::init_tracing("recipes=info");
    let store = Store::open(keep_client()?).await?;
    let imported = import::run(&store, &from).await?;
    println!("imported {imported} recipe(s) into keep");
    Ok(())
}
