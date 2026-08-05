//! mirror — a public, read-only view of a published Fizzy board.
//!
//! Fizzy runs in Docker on the `fedora-1` laptop, reachable only on the
//! tailnet. The goal is to let anyone see what the author is working on
//! without letting anyone change it, and without putting a laptop at home on
//! the public internet.
//!
//! # Why a separate service and not a proxy
//!
//! Fizzy has a publication feature of its own — `/public/boards/<key>` renders
//! a board to anonymous readers — and the obvious plan is to reverse-proxy
//! that from the VPS. Two things argue against it. Fizzy selects the app by
//! `Host` (kamal-proxy) while generating its absolute URLs from the *request*
//! host, so a second public hostname is either unroutable or produces links
//! back to an address no visitor can reach. And proxying means public traffic
//! terminates against the laptop: when it sleeps, the public page dies with
//! it.
//!
//! So mirror pulls instead. It holds its own snapshot on the VPS, taken over
//! the tailnet on an interval, and serves that. The laptop can sleep; the page
//! stays up and says how old it is. Nothing a visitor does reaches Fizzy.
//!
//! # Where the read-only guarantee lives
//!
//! In four places, each structural rather than a setting to get right:
//!
//! 1. The credential is a Fizzy access token with `read` permission —
//!    `Identity::AccessToken#allows?` rejects anything but GET and HEAD.
//! 2. [`fizzy`]'s types are narrower than Fizzy's JSON, so fields like a
//!    user's email address are dropped at the parse boundary and cannot reach
//!    the database.
//! 3. [`sanitize`] cleans rich text at ingest, so the stored snapshot is
//!    already safe to print.
//! 4. [`server`] exposes GETs over that snapshot and nothing else. There is no
//!    code path from a request to Fizzy.

mod assets;
mod fizzy;
mod render;
mod sanitize;
mod schema;
mod server;
mod store;
mod sync;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use fleet_common::util::{default_db_path, env_or};

#[derive(Parser)]
#[command(name = "mirror", about = "A public, read-only mirror of a Fizzy board")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server and the sync loop. (The default with no subcommand.)
    Serve,
    /// Run one sync pass and exit, reporting what it found. For checking a
    /// deployment without waiting out the interval.
    Sync,
    /// Empty the mirror: drop every board and every cached image.
    ///
    /// The operator's kill switch. Unpublishing a board in Fizzy removes it
    /// from the mirror on the next pass — but only if the laptop is awake to
    /// be asked. This does not need it to be.
    Purge,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None | Some(Command::Serve) => serve().await,
        Some(Command::Sync) => sync_once().await,
        Some(Command::Purge) => purge(),
    }
}

struct Settings {
    db_path: PathBuf,
    assets_dir: PathBuf,
    addr: SocketAddr,
    interval: Duration,
    site: render::Site,
    fizzy_base: String,
    fizzy_account: String,
    token_file: PathBuf,
}

fn settings() -> Result<Settings> {
    let db_path = std::env::var("MIRROR_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path("mirror", "mirror.db"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let assets_dir = std::env::var("MIRROR_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path("mirror", "assets"));
    Ok(Settings {
        db_path,
        assets_dir,
        addr: env_or("MIRROR_ADDR", "127.0.0.1:8103")
            .parse()
            .context("MIRROR_ADDR must be host:port")?,
        interval: Duration::from_secs(
            env_or("MIRROR_SYNC_INTERVAL_SECS", "300")
                .parse()
                .context("MIRROR_SYNC_INTERVAL_SECS must be a whole number of seconds")?,
        ),
        site: render::Site {
            name: env_or("MIRROR_SITE_NAME", "board"),
            public_url: env_or("MIRROR_PUBLIC_URL", "https://board.deepwa7er.com"),
        },
        fizzy_base: env_or("MIRROR_FIZZY_BASE", "https://fizzy.intern.deepwa7er.net"),
        // Fizzy mounts itself under a numeric account slug; without the prefix
        // every request 302s to the sign-in menu. See fizzy.rs.
        fizzy_account: env_or("MIRROR_FIZZY_ACCOUNT", "1"),
        token_file: PathBuf::from(env_or("MIRROR_FIZZY_TOKEN_FILE", "/etc/mirror/fizzy-token")),
    })
}

/// The token lives in a file, not an environment variable: `/proc/<pid>/environ`
/// and `systemctl show` both hand out a unit's environment, while the file is
/// 0600 and owned by the service user.
fn read_token(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("reading the Fizzy token from {}", path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("the Fizzy token at {} is empty", path.display());
    }
    Ok(token)
}

fn build(settings: &Settings) -> Result<sync::Deps> {
    let store = Arc::new(store::Store::open(&settings.db_path)?);
    let cache = Arc::new(assets::Cache::new(&settings.assets_dir).with_context(|| {
        format!(
            "opening the asset cache at {}",
            settings.assets_dir.display()
        )
    })?);
    let client = Arc::new(fizzy::Client::new(
        &settings.fizzy_base,
        &settings.fizzy_account,
        read_token(&settings.token_file)?,
        // Generous: the request crosses the tailnet to a laptop that may be
        // busy waking up. A slow sync is fine; the previous snapshot serves.
        Duration::from_secs(30),
    )?);
    Ok(sync::Deps {
        store,
        client,
        cache,
    })
}

async fn serve() -> Result<()> {
    fleet_common::http::init_tracing("mirror=info,tower_http=info");
    let settings = settings()?;
    let deps = build(&settings)?;
    tracing::info!(
        "database at {} · assets at {} · source {}",
        settings.db_path.display(),
        settings.assets_dir.display(),
        settings.fizzy_base
    );

    let app = Arc::new(server::App {
        store: deps.store.clone(),
        cache: deps.cache.clone(),
        site: settings.site,
    });
    // The sync loop runs beside the server rather than on a timer unit: it
    // shares the store, and a failed pass must leave the served snapshot
    // untouched, which is easiest to guarantee in one process.
    tokio::spawn(sync::run(deps, settings.interval));
    server::run(app, settings.addr).await?;
    Ok(())
}

async fn sync_once() -> Result<()> {
    fleet_common::http::init_tracing("mirror=info");
    let settings = settings()?;
    let deps = build(&settings)?;
    let pass = sync::once(&deps).await?;
    println!(
        "boards {} · cards {} · images {} · assets pruned {}",
        pass.boards, pass.cards, pass.images, pass.assets_removed
    );
    if pass.unstripped > 0 {
        println!(
            "warning: {} image(s) are in a format whose metadata could not be stripped — \
             they are published with whatever the camera recorded",
            pass.unstripped
        );
    }
    if pass.boards == 0 {
        println!("note: no board is published in Fizzy, so the mirror is empty");
    }
    Ok(())
}

fn purge() -> Result<()> {
    let settings = settings()?;
    let store = store::Store::open(&settings.db_path)?;
    store.purge()?;
    let cache = assets::Cache::new(&settings.assets_dir)?;
    let removed = cache.retain(&store.referenced_assets()?)?;
    println!("mirror emptied · {removed} cached image(s) deleted");
    println!("note: the next sync will re-publish whatever is still published in Fizzy");
    Ok(())
}
