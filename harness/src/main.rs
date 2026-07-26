//! harness — CLI entry point.
//!
//! No subcommand: the terminal REPL (or one-shot --prompt). `harness serve`:
//! the tailnet web UI. Both are frontends over the harness library crate.

use clap::{Parser, Subcommand};

mod render;
mod repl;
mod serve;

#[derive(Parser)]
#[command(
    name = "harness",
    about = "minimal Kimi coding harness",
    version,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    repl: repl::ReplArgs,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the web UI, bound to this Mac's Tailscale IP (tailnet-only, no auth).
    Serve(serve::ServeArgs),
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Serve(args)) => {
            if let Err(e) = serve::serve(args).await {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        None => repl::run(cli.repl).await,
    }
}
