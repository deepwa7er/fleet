use anyhow::Result;
use clap::Parser;
use tokio::sync::broadcast;

use skiff::config::Config;
use skiff::ingest::Ingest;
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
    Ingest::new(store.clone(), config.pi_dir(), topics.clone()).spawn();

    let router = router(AppState::new(store, topics), config.web_dist_path());
    serve(config.addr, router).await
}
