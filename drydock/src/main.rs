mod cli;
mod core;
mod error;
mod server;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::json;

use cli::{Client, CliError};
use core::Store;

#[derive(Parser)]
#[command(name = "drydock", about = "Ticket queue for autonomous fleet work")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server: web view + JSON API.
    Serve,

    /// Print the next actionable ticket (reclaims stale claims first).
    Next {
        #[arg(long)]
        json: bool,
    },

    /// Claim a ticket for work (open -> in-progress). Exit code 2 if already claimed.
    Claim {
        id: i64,
        #[arg(long)]
        branch: String,
    },

    /// Show a ticket with its full thread.
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },

    /// Park a ticket on a question for the human (-> needs-input).
    NeedsInput { id: i64, question: String },

    /// Mark a ticket blocked. Do NOT hack around a wall — block and explain instead.
    Block { id: i64, reason: String },

    /// Resolve a ticket by linking the opened PR (-> in-review).
    Resolve {
        id: i64,
        #[arg(long)]
        pr: String,
    },

    /// Create a ticket (normally done from the web view).
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        goal: String,
        #[arg(long, default_value = "med")]
        priority: String,
        #[arg(long)]
        acceptance: Option<String>,
        #[arg(long)]
        constraints: Option<String>,
    },
}

fn env_or(key: &str, default: impl Into<String>) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

/// Default DB path: $XDG_DATA_HOME/drydock/drydock.db (falling back to ~/.local/share).
fn default_db_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join("drydock").join("drydock.db")
}

fn run_serve() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "drydock=info,tower_http=info".into()),
        )
        .init();

    let db_path = std::env::var("DRYDOCK_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path());
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Arc::new(Store::open(&db_path)?);
    tracing::info!("database at {}", db_path.display());

    let config = server::ServerConfig {
        addr: env_or("DRYDOCK_ADDR", "127.0.0.1:7878").parse::<SocketAddr>()?,
        web_dir: PathBuf::from(env_or("DRYDOCK_WEB_DIR", "web/dist")),
        stale_hours: env_or("DRYDOCK_STALE_HOURS", "3").parse()?,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::run(store, config))?;
    Ok(())
}

fn run_client(command: Command) -> i32 {
    let client = Client::from_env();
    let result = match command {
        Command::Next { json } => client
            .get("/api/tickets/next")
            .map(|v| cli::print_next(&v, json)),
        Command::Show { id, json } => client
            .get(&format!("/api/tickets/{id}"))
            .map(|v| cli::print_show(&v, json)),
        Command::Claim { id, branch } => client
            .post(&format!("/api/tickets/{id}/claim"), json!({ "branch": branch }))
            .map(|v| cli::print_one(&v, "claimed")),
        Command::NeedsInput { id, question } => client
            .post(
                &format!("/api/tickets/{id}/needs-input"),
                json!({ "body": question }),
            )
            .map(|v| cli::print_one(&v, "parked for input")),
        Command::Block { id, reason } => client
            .post(&format!("/api/tickets/{id}/block"), json!({ "body": reason }))
            .map(|v| cli::print_one(&v, "blocked")),
        Command::Resolve { id, pr } => client
            .post(&format!("/api/tickets/{id}/resolve"), json!({ "pr_url": pr }))
            .map(|v| cli::print_one(&v, "resolved")),
        Command::Create {
            title,
            target,
            goal,
            priority,
            acceptance,
            constraints,
        } => client
            .post(
                "/api/tickets",
                json!({
                    "title": title,
                    "target": target,
                    "goal": goal,
                    "priority": priority,
                    "acceptance": acceptance,
                    "constraints": constraints,
                }),
            )
            .map(|v| cli::print_one(&v, "created")),
        Command::Serve => unreachable!("serve handled separately"),
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            CliError::exit_code(&e)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Command::Serve = cli.command {
        if let Err(e) = run_serve() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    } else {
        std::process::exit(run_client(cli.command));
    }
}
