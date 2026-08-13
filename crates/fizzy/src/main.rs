//! `fizzy` — create and list Fizzy (Once) cards from the shell.
//!
//! The agent's write path: `cargo run -p fizzy -- create --board Playground --title "…" --body-file /tmp/card.md`.
//! Reads the write token from `FIZZY_TOKEN_FILE` (default `~/.config/fizzy/write-token`), not from env —
//! `/proc/<pid>/environ` and `systemctl show` both leak env, while a 0600 file does not (same rationale as
//! `MIRROR_FIZZY_TOKEN_FILE` in `mirror.service`).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use fizzy::{Client, Standing};

#[derive(Parser)]
#[command(name = "fizzy", about = "Fizzy (Once) card client — list boards and create triage cards")]
struct Cli {
    /// Origin, e.g. https://fizzy.intern.deepwa7er.net (no trailing slash, no account).
    #[arg(long, env = "FIZZY_BASE", default_value = "https://fizzy.intern.deepwa7er.net")]
    base: String,

    /// Numeric account slug Fizzy mounts under. Without it every request 302s to /session/menu.
    #[arg(long, env = "FIZZY_ACCOUNT", default_value = "1")]
    account: String,

    /// File containing the Bearer token (write permission for `create`).
    #[arg(long, env = "FIZZY_TOKEN_FILE")]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List boards visible to the token.
    Boards,
    /// List triage (stream) cards for a board — the set `cargo run -p fizzy -- create` writes into.
    Stream {
        /// Board id or exact name (e.g. `Playground` or `03gmeh5pknsd4ycijtmjxy4td`).
        #[arg(long)]
        board: String,
    },
    /// Show one card in full — title, status, board, and body.
    Show {
        /// Card number, as it appears in the URL `/1/cards/<number>` (with or without a leading `#`).
        number: String,
    },
    /// Create a published card in a board's triage.
    Create {
        /// Board id or exact name.
        #[arg(long)]
        board: String,
        /// Card title. Empty titles become "Untitled" server-side, but we reject them client-side.
        #[arg(long)]
        title: String,
        /// Card body (ActionText description). Plain markdown is fine.
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from a file (useful for multi-line markdown).
        #[arg(long, conflicts_with = "body")]
        body_file: Option<PathBuf>,
        /// If set, don't POST — print what would be sent and exit 0.
        #[arg(long)]
        dry_run: bool,
        /// If a published triage card with the same title already exists, skip creation and exit 0.
        #[arg(long)]
        dedupe: bool,
    },
}

fn default_token_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/fizzy/write-token")
}

fn read_token(path: &PathBuf) -> Result<String> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading Fizzy token from {}", path.display()))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        anyhow::bail!("Fizzy token at {} is empty", path.display());
    }
    Ok(s)
}

fn client(args: &Cli) -> Result<Client> {
    let token_file = args
        .token_file
        .clone()
        .unwrap_or_else(default_token_file);
    let token = read_token(&token_file)?;
    Client::new(&args.base, &args.account, token, Duration::from_secs(15))
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let c = client(&cli)?;

    match cli.command {
        Command::Boards => {
            let boards = c.boards().await?;
            if boards.is_empty() {
                println!("no boards visible to this token");
            } else {
                for b in boards {
                    let pub_flag = if b.public_url.is_some() { "  published" } else { "" };
                    println!("{}  {}{}", b.name, b.id, pub_flag);
                }
            }
        }
        Command::Stream { board } => {
            let b = c.resolve_board(&board).await?;
            let cards = c.standing_cards(&b.id, Standing::Triage).await?;
            if cards.is_empty() {
                println!("no triage cards in {} ({})", b.name, b.id);
            } else {
                for card in cards {
                    println!("#{}  {}  {}", card.number, card.title, card.status);
                }
            }
        }
        Command::Show { number } => {
            let n: i64 = number
                .trim()
                .trim_start_matches('#')
                .parse()
                .with_context(|| format!("card number must be an integer, got {number:?}"))?;
            let card = c.card(n).await?;
            println!("#{}  {}", card.number, card.title);
            println!("status: {}", card.status);
            if let Some(b) = &card.board {
                println!("board: {} ({})", b.name, b.id);
            }
            if let Some(u) = &card.creator {
                println!("creator: {}", u.name);
            }
            println!("{}/cards/{}", c.base(), card.number);
            if !card.description.trim().is_empty() {
                println!("--- body ---");
                println!("{}", card.description);
                println!("--- end body ---");
            }
        }
        Command::Create {
            board,
            title,
            body,
            body_file,
            dry_run,
            dedupe,
        } => {
            let b = c.resolve_board(&board).await?;
            let body_text = if let Some(p) = body_file {
                std::fs::read_to_string(&p)
                    .with_context(|| format!("reading body from {}", p.display()))?
            } else {
                body.unwrap_or_default()
            };

            if dedupe {
                let existing = c.standing_cards(&b.id, Standing::Triage).await?;
                if existing
                    .iter()
                    .any(|card| card.status == "published" && card.title == title)
                {
                    println!("skip: triage card with title {title:?} already exists in {} ({})", b.name, b.id);
                    return Ok(());
                }
            }

            if dry_run {
                println!("dry-run: would POST {}/boards/{}/cards.json", c.base(), b.id);
                println!("board: {} ({})", b.name, b.id);
                println!("title: {}", title);
                println!("--- body ---");
                println!("{}", body_text);
                println!("--- end body ---");
                return Ok(());
            }

            let card = c.create_card(&b.id, &title, &body_text).await?;
            // Fizzy's canonical card URL is <origin>/<account>/cards/:number — and
            // `base` is exactly origin/account, e.g. https://fizzy.intern.deepwa7er.net/1.
            println!("created #{}: {}", card.number, card.title);
            println!("{}/cards/{}", c.base(), card.number);
            if let Some(board) = card.board {
                println!("board: {} ({})", board.name, board.id);
            }
        }
    }
    Ok(())
}
