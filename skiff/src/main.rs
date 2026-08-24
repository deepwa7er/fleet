use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;

use skiff::config::Config;
use skiff::ingest::{Ingest, Topic};
use skiff::run::Runs;
use skiff::server::{AppState, router, serve};
use skiff::store::Store;

/// How many topic announcements a slow client may fall behind before it is
/// told to resnapshot. Generous, because falling behind costs the client a
/// redundant recompute and nothing else.
const TOPIC_BUFFER: usize = 256;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "skiff=info,tower_http=warn".into()),
        )
        .init();

    let config = Config::parse();
    let store = Store::open(&config.store_path())?;
    let (topics, _) = broadcast::channel(TOPIC_BUFFER);

    // The ingest owns its own thread for the process's lifetime; it is the
    // thing that makes every view non-empty, so there is nothing to gracefully
    // shut down that outliving the process would help.
    Ingest::new(store.clone(), config.sources(), topics.clone()).spawn();

    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        config.pi_binary(),
        config.pi_dir(),
        config.pi_session_dir.is_some(),
    );

    // A finished reply is handed over to the transcript, and a sent prompt
    // retired, when the session's file actually changes — not when pi says so.
    // See `Runs::session_changed`.
    tokio::spawn({
        let runs = runs.clone();
        let mut topics = topics.subscribe();
        async move {
            loop {
                match topics.recv().await {
                    Ok(Topic::Session(id)) => runs.session_changed(&id).await,
                    Ok(_) => {}
                    // Falling behind only delays a handover; the next file
                    // change resolves it.
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });

    let router = router(AppState::new(store, runs, topics), config.web_dist_path());
    serve(config.addr, router).await
}
