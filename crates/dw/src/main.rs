//! `dw` — the terminal client of the shared change domain.
//!
//! No daemon or password is required to read or author a round. Skiff is the
//! review surface and sole landing caller; `dw` creates, curates, and submits.

mod jj;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use change::{
    default_change_dir, default_repos_dir, AnnotationSide, Author, Change, ChangeService,
    ChangeState, Jj, RoundInput, Store,
};
use clap::{Parser, ValueEnum};

#[derive(Parser)]
#[command(name = "dw", about = "The one-noun source-control workflow (DW-002)")]
enum Cli {
    /// What's happening, plus confirmation that your working copy is safe.
    Status,
    /// Print the checkout path for a card's change.
    Edit {
        card: u64,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Create the change record for an existing Fizzy card.
    Start {
        card: u64,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Rebind an existing change to the agent session working on it.
    Bind {
        card: u64,
        session: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Register one additive jj commit as the next review round.
    Round {
        card: u64,
        /// Revision carrying the finished round. The workflow moves @ to a
        /// fresh commit first, so the previous commit is the default.
        #[arg(long, default_value = "@-")]
        revision: String,
        #[arg(long, value_enum, default_value = "agent")]
        author: AuthorArg,
        #[arg(long)]
        note: Option<String>,
        #[arg(long = "gate")]
        gates_ran: Vec<String>,
        #[arg(long = "worth-knowing")]
        worth_knowing: Vec<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Attach a justification to an exact line in one round's diff.
    Annotate {
        card: u64,
        #[arg(long)]
        round: u32,
        #[arg(long)]
        path: String,
        #[arg(long)]
        line: u32,
        #[arg(long, value_enum, default_value = "new")]
        side: SideArg,
        #[arg(long)]
        text: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Put a change in front of the human at the Skiff desk.
    Submit {
        card: u64,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Explicitly retry unfinished post-landing consequences.
    Finish {
        card: u64,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Turn your own working copy into a one-round change ready to land.
    Ship {
        /// What this is — the only ceremony left for your own work.
        sentence: String,
        #[arg(long, env = "DW_BOARD", default_value = "Playground")]
        board: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum AuthorArg {
    Agent,
    You,
}

impl From<AuthorArg> for Author {
    fn from(author: AuthorArg) -> Self {
        match author {
            AuthorArg::Agent => Self::Agent,
            AuthorArg::You => Self::You,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SideArg {
    Old,
    New,
}

impl From<SideArg> for AnnotationSide {
    fn from(side: SideArg) -> Self {
        match side {
            SideArg::Old => Self::Old,
            SideArg::New => Self::New,
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = match std::env::args().len() {
        1 => Cli::Status,
        _ => Cli::parse(),
    };
    if let Err(error) = run(cli).await {
        eprintln!("dw: {error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let service = service()?;
    match cli {
        Cli::Status => status(&service),
        Cli::Edit { card, repo } => edit(&service, repo, card),
        Cli::Start {
            card,
            title,
            session,
            repo,
        } => {
            let repo = repo_name(repo)?;
            service.create(&repo, card, title.as_deref(), session.as_deref())?;
            println!("created {repo} #{card}");
            Ok(())
        }
        Cli::Bind {
            card,
            session,
            repo,
        } => {
            let repo = repo_name(repo)?;
            service.set_session(&repo, card, &session)?;
            println!("bound {repo} #{card} to {session}");
            Ok(())
        }
        Cli::Round {
            card,
            revision,
            author,
            note,
            gates_ran,
            worth_knowing,
            repo,
        } => {
            let repo = repo_name(repo)?;
            let change_id = service.change_id(&repo, &revision)?;
            let round = service.add_round(
                &repo,
                card,
                RoundInput {
                    author: author.into(),
                    change_id,
                    note,
                    gates_ran,
                    worth_knowing,
                },
            )?;
            println!(
                "registered {repo} #{card} round {} ({})",
                round.n, round.change_id
            );
            Ok(())
        }
        Cli::Annotate {
            card,
            round,
            path,
            line,
            side,
            text,
            repo,
        } => {
            let repo = repo_name(repo)?;
            let annotation =
                service.add_annotation(&repo, card, round, &path, line, side.into(), &text)?;
            println!("annotated {repo} #{card} round {round} ({})", annotation.id);
            Ok(())
        }
        Cli::Submit { card, repo } => {
            let repo = repo_name(repo)?;
            service.transition(&repo, card, ChangeState::InReview)?;
            println!("submitted {repo} #{card} · review in Skiff");
            Ok(())
        }
        Cli::Finish { card, repo } => {
            let repo = repo_name(repo)?;
            let landing =
                change::LandingService::new(service.clone(), change::LandingConfig::from_env())?;
            let report = landing.finish(&repo, card).await?;
            println!(
                "finished {repo} #{card} · record {} · card comment {} · deploy {} · {} job outcomes",
                attempted(report.record_attempted),
                attempted(report.card_comment_attempted),
                attempted(report.deploy_triggered),
                report.deploy_jobs_finished,
            );
            Ok(())
        }
        Cli::Ship { sentence, board } => ship(&service, &sentence, &board).await,
    }
}

fn attempted(value: bool) -> &'static str {
    if value {
        "retried"
    } else {
        "complete/disabled"
    }
}

fn service() -> Result<ChangeService> {
    let changes = match std::env::var_os("DW_CHANGE_DIR") {
        Some(path) => PathBuf::from(path),
        None => default_change_dir()?,
    };
    let repos = default_repos_dir()?;
    let binary = std::env::var_os("JJ_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| "jj".into());
    Ok(ChangeService::new(
        Store::new(changes),
        repos,
        Jj::new(binary),
    ))
}

fn repo_name(explicit: Option<String>) -> Result<String> {
    if let Some(repo) = explicit {
        return Ok(repo);
    }
    Ok(jj::containing_repo(&std::env::current_dir()?)
        .context("run inside a jj repository or pass --repo")?
        .name)
}

fn status(service: &ChangeService) -> Result<()> {
    let changes = service.list()?;
    let (waiting, running): (Vec<_>, Vec<_>) = changes
        .iter()
        .filter(|change| change.state != ChangeState::Shipped)
        .partition(|change| matches!(change.state, ChangeState::InReview | ChangeState::Landing));
    println!();
    println!(
        "  waiting on you{}",
        if waiting.is_empty() {
            ""
        } else {
            "                    review in Skiff"
        }
    );
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
    let title = change
        .title
        .clone()
        .unwrap_or_else(|| format!("change #{}", change.card));
    let state = match change.state {
        ChangeState::InReview => String::new(),
        other => format!(" · {other}").replace('_', " "),
    };
    format!(
        "#{}  {}      round {}{}",
        change.card,
        title,
        change.rounds.len(),
        state
    )
}

fn edit(service: &ChangeService, repo: Option<String>, card: u64) -> Result<()> {
    let repo = repo_name(repo)?;
    service.store().require(&repo, card)?;
    println!("{}", service.repository(&repo)?.display());
    Ok(())
}

async fn ship(service: &ChangeService, sentence: &str, board: &str) -> Result<()> {
    let sentence = sentence.trim();
    if sentence.is_empty() {
        bail!("ship takes a sentence — it is the only ceremony left");
    }
    let repo = jj::containing_repo(&std::env::current_dir()?)
        .context("dw ship runs inside the repository you are shipping")?;
    if repo.changed_paths()?.is_empty() {
        bail!(
            "nothing to ship — the working copy in {} is clean",
            repo.name
        );
    }
    service.repository(&repo.name)?;
    let change_id = repo.describe_working_copy(sentence)?;
    repo.new_working_copy()?;
    let card = create_card(board, sentence).await?;
    println!("card #{card} · {sentence}");
    service.create(&repo.name, card, Some(sentence), None)?;
    service.add_round(
        &repo.name,
        card,
        RoundInput {
            author: Author::You,
            change_id,
            note: None,
            gates_ran: Vec::new(),
            worth_knowing: Vec::new(),
        },
    )?;
    service.transition(&repo.name, card, ChangeState::InReview)?;
    println!("ready to land · approve #{card} in Skiff");
    Ok(())
}

async fn create_card(board: &str, sentence: &str) -> Result<u64> {
    let base =
        std::env::var("FIZZY_BASE").unwrap_or_else(|_| "https://fizzy.intern.deepwa7er.net".into());
    let account = std::env::var("FIZZY_ACCOUNT").unwrap_or_else(|_| "1".into());
    let token_file = match std::env::var("FIZZY_TOKEN_FILE") {
        Ok(path) => PathBuf::from(path),
        Err(_) => home()?.join(".config/fizzy/write-token"),
    };
    let token = std::fs::read_to_string(&token_file)
        .with_context(|| format!("cannot read the Fizzy token at {}", token_file.display()))?
        .trim()
        .to_owned();
    let client = fizzy::Client::new(&base, &account, token, Duration::from_secs(15))?;
    let board = client.resolve_board(board).await?;
    let body = fizzy::format::markdown_to_html(&format!(
        "Prepared with `dw ship` — your own work skips curation (DW-002 §7).\n\n---\n\nProvenance: dw ship, {}\n",
        chrono_free_date()
    ));
    let card = client.create_card(&board.id, sentence, &body).await?;
    u64::try_from(card.number).context("Fizzy answered a negative card number")
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME or FIZZY_TOKEN_FILE is required to find the Fizzy token")
}

fn chrono_free_date() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    civil((seconds / 86_400) as i64)
}

fn civil(days: i64) -> String {
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

    fn change(card: u64, title: &str, state: ChangeState, rounds: usize) -> Change {
        let mut change = Change {
            repo: "fleet".to_owned(),
            card,
            title: Some(title.to_owned()),
            session: None,
            state,
            created_at: "x".to_owned(),
            updated_at: "x".to_owned(),
            rounds: Vec::new(),
            last_request: None,
            landed: None,
            last_landing: None,
            card_comment: None,
            record_export: None,
            deploy: None,
            path: None,
        };
        for n in 1..=rounds {
            change.rounds.push(change::Round {
                n: n as u32,
                author: Author::Agent,
                change_id: "k".repeat(32),
                note: None,
                gates_ran: Vec::new(),
                worth_knowing: Vec::new(),
                created_at: "x".to_owned(),
                annotations: Vec::new(),
                commit: None,
                divergent: false,
            });
        }
        change
    }

    #[test]
    fn status_line_reads_like_the_design() {
        assert_eq!(
            line(&change(81, "pi model picker", ChangeState::InReview, 2)),
            "#81  pi model picker      round 2"
        );
        assert_eq!(
            line(&change(84, "shutter crash fix", ChangeState::Landing, 1)),
            "#84  shutter crash fix      round 1 · landing"
        );
    }

    #[test]
    fn civil_date_examples() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(20_688), "2026-08-23");
    }
}
