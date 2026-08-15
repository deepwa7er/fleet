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

use fizzy::format;
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
    /// Scaffold a well-formatted card body to a file.
    ///
    /// Writes the `Why / Evidence / Options / Provenance` template that
    /// `create --body-file` expects. Edit the file, then post it. This is
    /// the preferred way to create cards — it guarantees blank lines around
    /// headings and consistent sections so the card renders correctly in
    /// Fizzy's Trix/ActionText view.
    Draft {
        /// Card title — used to slug the default output path.
        #[arg(long)]
        title: String,
        /// Where to write the draft. Defaults to `/tmp/<slug>.md`.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Overwrite an existing file at the output path.
        #[arg(long)]
        force: bool,
    },
    /// Validate a card body without posting.
    ///
    /// Checks the same `Why / Evidence` scaffold that `create` enforces for
    /// `fleet:`-prefixed titles and prints the normalised body. Useful for
    /// agents to lint before `create`.
    Lint {
        /// Card title (affects strictness — `fleet:` titles require Why/Evidence).
        #[arg(long)]
        title: String,
        /// Card body (ActionText description).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from a file.
        #[arg(long, conflicts_with = "body")]
        body_file: Option<PathBuf>,
    },
    /// Create a published card in a board's triage.
    Create {
        /// Board id or exact name.
        #[arg(long)]
        board: String,
        /// Card title. Empty titles become "Untitled" server-side, but we reject them client-side.
        #[arg(long)]
        title: String,
        /// Card body (ActionText description). Prefer `--body-file` with a file from `fizzy draft`.
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from a file (preferred — preserves newlines and markdown structure).
        #[arg(long, conflicts_with = "body")]
        body_file: Option<PathBuf>,
        /// If set, don't POST — print what would be sent and exit 0.
        #[arg(long)]
        dry_run: bool,
        /// If a published triage card with the same title already exists, skip creation and exit 0.
        #[arg(long)]
        dedupe: bool,
        /// Skip scaffold validation (still normalises markdown). Use for free-form idea cards.
        #[arg(long)]
        raw: bool,
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

fn client_with(base: &str, account: &str, token_file: &Option<PathBuf>) -> Result<Client> {
    let path = token_file
        .clone()
        .unwrap_or_else(default_token_file);
    let token = read_token(&path)?;
    Client::new(base, account, token, Duration::from_secs(15))
}

#[allow(dead_code)]
fn client(args: &Cli) -> Result<Client> {
    client_with(&args.base, &args.account, &args.token_file)
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let Cli {
        base,
        account,
        token_file,
        command,
    } = Cli::parse();

    match command {
        Command::Boards => {
            let c = client_with(&base, &account, &token_file)?;
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
            let c = client_with(&base, &account, &token_file)?;
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
            let c = client_with(&base, &account, &token_file)?;
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
        Command::Draft { title, output, force } => {
            // Draft does not need a client or network; it just scaffolds a file.
            format::validate_title(&title)?;
            let template = format::card_template();
            let slug = format::slugify(&title);
            let path = output.unwrap_or_else(|| PathBuf::from(format!("/tmp/{slug}.md")));
            if path.exists() && !force {
                anyhow::bail!(
                    "draft file {} already exists — use --force to overwrite or --output to pick another path",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("creating draft dir {}", parent.display()))?;
                }
            }
            std::fs::write(&path, &template)
                .with_context(|| format!("writing draft to {}", path.display()))?;
            println!("draft: {title:?} → {}", path.display());
            println!("edit {} then run:", path.display());
            println!(
                "  cargo run -p fizzy -- create --board Playground --title {title:?} --body-file {}",
                path.display()
            );
        }
        Command::Lint {
            title,
            body,
            body_file,
        } => {
            format::validate_title(&title)?;
            let raw = if let Some(p) = body_file {
                std::fs::read_to_string(&p)
                    .with_context(|| format!("reading body from {}", p.display()))?
            } else {
                body.unwrap_or_default()
            };
            let normalised = format::normalize_body(&raw);
            let warnings = format::validate_body(&normalised, &title);
            if warnings.is_empty() {
                println!("lint ok: title and body look well-formed");
            } else {
                eprintln!("lint warnings for {title:?}:");
                for w in &warnings {
                    eprintln!("  - {w}");
                }
            }
            println!("--- body (normalised) ---");
            println!("{normalised}");
            println!("--- end body ---");
            let is_hard_fail = warnings.iter().any(|w| w.contains("## Why") || w.contains("## Evidence"));
            if is_hard_fail {
                anyhow::bail!("lint failed — fix the warnings above or re-run with --raw on create");
            } else if !warnings.is_empty() {
                eprintln!("(soft warnings — add Provenance/title dash for best practice)");
            }
        }
        Command::Create {
            board,
            title,
            body,
            body_file,
            dry_run,
            dedupe,
            raw,
        } => {
            // Title is always validated; body structure is validated unless --raw.
            format::validate_title(&title)?;

            let is_body_flag = body.is_some();
            let raw_body = if let Some(p) = body_file {
                std::fs::read_to_string(&p)
                    .with_context(|| format!("reading body from {}", p.display()))?
            } else {
                body.unwrap_or_default()
            };

            if is_body_flag {
                eprintln!(
                    "warning: --body is deprecated — use `fizzy draft --title {title:?}` and --body-file for reliable formatting (preserves newlines, headings, lists)"
                );
            }

            let body_text = format::normalize_body(&raw_body);
            let warnings = format::validate_body(&body_text, &title);

            if !raw && !warnings.is_empty() {
                let is_hard = warnings.iter().any(|w| w.contains("## Why") || w.contains("## Evidence"));
                if is_hard {
                    eprintln!("validation failed for {title:?}:");
                    for w in &warnings {
                        eprintln!("  - {w}");
                    }
                    eprintln!("hint: run `cargo run -p fizzy -- draft --title {title:?}` to scaffold, or `cargo run -p fizzy -- lint --title {title:?} --body-file <path>` to check, or pass --raw to skip this check");
                    anyhow::bail!("aborted — body missing required sections (use --raw to post anyway)");
                } else {
                    for w in &warnings {
                        eprintln!("warning: {w}");
                    }
                }
            }

            let c = client_with(&base, &account, &token_file)?;
            let b = c.resolve_board(&board).await?;

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
                if !warnings.is_empty() && raw {
                    eprintln!("(validation skipped via --raw; would have warned:)");
                    for w in &warnings {
                        eprintln!("  - {w}");
                    }
                }
                println!("--- body (normalised) ---");
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
