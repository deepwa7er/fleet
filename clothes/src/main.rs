mod core;
mod error;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use core::Store;

#[derive(Parser)]
#[command(name = "clothes", about = "Wardrobe organizer for building a classic wardrobe")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server: web view + JSON API. (The default with no subcommand.)
    Serve,
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

/// Default DB path: $XDG_DATA_HOME/clothes/clothes.db (falling back to
/// ~/.local/share).
fn default_db_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join("clothes").join("clothes.db")
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "clothes=info,tower_http=info".into()),
        )
        .init();

    let db_path = std::env::var("CLOTHES_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path());
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Arc::new(Store::open(&db_path)?);
    tracing::info!("database at {}", db_path.display());

    let config = server::ServerConfig {
        addr: env_or("CLOTHES_ADDR", "127.0.0.1:8099").parse::<SocketAddr>()?,
        web_dir: PathBuf::from(env_or("CLOTHES_WEB_DIR", "web/dist")),
    };

    server::run(store, config).await?;
    Ok(())
}
