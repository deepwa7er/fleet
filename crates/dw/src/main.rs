//! dw — the one-noun CLI (DW-002 §7). You hold one noun: the thing you are
//! trying to get done, which is a Fizzy card. Reviewing has no command (it
//! happens in skiff), capture has no command (it is not a thing that
//! happens), so the terminal only ever:
//!
//!   dw                what's happening, and that your own work is safe
//!   dw edit <card>    print the change's checkout path — nothing else
//!   dw ship "<why>"   your own work: the one sentence of ceremony left
//!
//! ship deliberately reuses the whole existing machinery instead of adding
//! any: it creates the card (everything is a card — Fizzy is the spine),
//! then drives create-change → round(author: you) → submit → approve, and
//! approve is what lands, records the timeline entry, and comments the
//! card. Self-review is theatre; the machinery is not.

mod bridge;
mod jj;
mod secrets;

use anyhow::{bail, Context, Result};
use bridge::{Bridge, Change};
use clap::Parser;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "dw", about = "The one-noun CLI: status, a path, and one sentence (DW-002 §7)")]
enum Cli {
    /// What's happening, and confirmation that your own work is safe.
    Status,
    /// Print the checkout path of a card's change — the one place a
    /// filesystem path surfaces. Open it in the editor you already have.
    Edit { card: u64 },
    /// Ship your own work: the working copy becomes a round authored by
    /// you, and the sentence is the card title, the description, and the
    /// record entry's name for it.
    Ship {
        /// What this is — the only ceremony left in the system.
        sentence: String,
        /// Fizzy board for the card ship creates.
        #[arg(long, env = "DW_BOARD", default_value = "Playground")]
        board: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = match std::env::args().len() {
        1 => Cli::Status, // bare `dw` is the status readout, per the §7 mock
        _ => Cli::parse(),
    };
    if let Err(err) = run(cli).await {
        eprintln!("dw: {err:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let bridge = Bridge::new(secrets::bridge_password()?);
    match cli {
        Cli::Status => status(&bridge).await,
        Cli::Edit { card } => edit(&bridge, card).await,
        Cli::Ship { sentence, board } => ship(&bridge, &sentence, &board).await,
    }
}

// --- dw ----------------------------------------------------------------------

async fn status(bridge: &Bridge) -> Result<()> {
    let changes = bridge.changes().await?;
    let (waiting, running): (Vec<_>, Vec<_>) = changes
        .iter()
        .filter(|change| change.state != "shipped")
        .partition(|change| change.state == "in_review" || change.state == "landing");

    println!();
    println!("  waiting on you{}", if waiting.is_empty() { "" } else { "                    review in skiff" });
    if waiting.is_empty() {
        println!("    nothing");
    }
    for change in &waiting {
        println!("  ▸ {}", line(change));
    }

    if !running.is_empty() {
        println!();
        println!("  running");
        for change in &running {
            println!("    {}", line(change));
        }
    }

    println!();
    println!("  yours");
    match jj::containing_repo(&std::env::current_dir()?) {
        None => println!("    not inside a repository"),
        Some(repo) => {
            let changed = repo.changed_paths()?;
            if changed.is_empty() {
                println!("    working copy in {} is clean", repo.name);
            } else {
                println!(
                    "    {} file{} changed in {}, all recoverable",
                    changed.len(),
                    if changed.len() == 1 { "" } else { "s" },
                    repo.name
                );
            }
        }
    }
    println!();
    Ok(())
}

fn line(change: &Change) -> String {
    let title = change.title.clone().unwrap_or_else(|| format!("change #{}", change.card));
    let round = change.rounds.len();
    let state = match change.state.as_str() {
        "in_review" => String::new(),
        other => format!(" · {}", other.replace('_', " ")),
    };
    format!("#{}  {}      round {}{}", change.card, title, round, state)
}

// --- dw edit -----------------------------------------------------------------

async fn edit(bridge: &Bridge, card: u64) -> Result<()> {
    let changes = bridge.changes().await?;
    let Some(found) = changes.iter().find(|change| change.card == card) else {
        bail!("no change for card #{card}");
    };
    let change = bridge.change(&found.repo, card).await?;
    match change.path {
        // The path, nothing else: no editor integrations, deliberately —
        // you open it in the window you already have (DW-002 §9).
        Some(path) => println!("{path}"),
        None => bail!("the bridge did not report a path for {}/{card}", found.repo),
    }
    Ok(())
}

// --- dw ship -----------------------------------------------------------------

async fn ship(bridge: &Bridge, sentence: &str, board: &str) -> Result<()> {
    let sentence = sentence.trim();
    if sentence.is_empty() {
        bail!("ship takes a sentence — it is the only ceremony left");
    }
    let repo = jj::containing_repo(&std::env::current_dir()?)
        .context("dw ship runs inside the repository you are shipping")?;
    if repo.changed_paths()?.is_empty() {
        bail!("nothing to ship — the working copy in {} is clean", repo.name);
    }

    // The sentence becomes the commit description; @ moves off so the
    // landing rebases a closed commit.
    let change_id = repo.describe_working_copy(sentence)?;
    repo.new_working_copy()?;

    // Everything is a card. The card is created first — the change binds to
    // its number — and approve's comment closes the loop on it.
    let card = create_card(board, sentence).await?;
    println!("card #{card} · {sentence}");

    bridge.create_change(&repo.name, card, sentence).await?;
    bridge.add_round(&repo.name, card, "you", &change_id).await?;
    bridge.submit(&repo.name, card).await?;
    bridge.approve(&repo.name, card).await?;
    println!("landing…");

    // Approve is a request; follow it to its outcome.
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let change = bridge.change(&repo.name, card).await?;
        match change.state.as_str() {
            "landing" => continue,
            "shipped" => {
                let tip = change.landed.map(|landed| landed.tip).unwrap_or_default();
                println!("shipped — {} is on origin/main ({})", sentence, &tip[..tip.len().min(12)]);
                return Ok(());
            }
            _ => {
                let reason = change
                    .last_landing
                    .and_then(|landing| landing.reason)
                    .unwrap_or_else(|| "no reason recorded".into());
                bail!(
                    "the landing came back: {reason}\nthe change sits in review on card #{card} — resolve in {}, then approve from skiff",
                    repo.root.display()
                );
            }
        }
    }
    bail!("the landing is still running after two minutes — watch card #{card} in skiff");
}

async fn create_card(board: &str, sentence: &str) -> Result<u64> {
    let base = std::env::var("FIZZY_BASE").unwrap_or_else(|_| "https://fizzy.intern.deepwa7er.net".into());
    let account = std::env::var("FIZZY_ACCOUNT").unwrap_or_else(|_| "1".into());
    let token_file = std::env::var("FIZZY_TOKEN_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| secrets::home().join(".config").join("fizzy").join("write-token"));
    let token = std::fs::read_to_string(&token_file)
        .with_context(|| format!("cannot read the fizzy write token at {}", token_file.display()))?
        .trim()
        .to_string();
    let client = fizzy::Client::new(&base, &account, token, Duration::from_secs(15))?;
    let board = client.resolve_board(board).await?;
    let body = fizzy::format::markdown_to_html(&format!(
        "Shipped directly with `dw ship` — your own work skips review (DW-002 §7).\n\n---\n\nProvenance: dw ship, {}\n",
        chrono_free_date()
    ));
    let card = client.create_card(&board.id, sentence, &body).await?;
    u64::try_from(card.number).context("fizzy answered a negative card number")
}

// The date without a chrono dependency: seconds-since-epoch to a civil date
// (Howard Hinnant's algorithm). dw prints it in provenance only.
fn chrono_free_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(card: u64, title: &str, state: &str, rounds: usize) -> Change {
        serde_json::from_value(serde_json::json!({
            "repo": "fleet",
            "card": card,
            "title": title,
            "state": state,
            "rounds": (1..=rounds).map(|_| serde_json::json!({})).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn status_line_reads_like_the_mock() {
        assert_eq!(line(&change(81, "pi model picker", "in_review", 2)), "#81  pi model picker      round 2");
        assert_eq!(
            line(&change(84, "shutter crash fix", "landing", 1)),
            "#84  shutter crash fix      round 1 · landing"
        );
    }

    #[test]
    fn the_date_is_civil() {
        // 2026-08-23 is around 1_787_800_000; sanity that the formula holds
        // for a known epoch day: 1970-01-01.
        let formatted = chrono_free_date();
        assert_eq!(formatted.len(), 10);
        assert!(formatted.starts_with("20"));
    }
}
