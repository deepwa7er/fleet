//! keep — the fleet's central database service.
//!
//! One axum server on OVH embedding the turso engine, holding one database
//! per fleet service. Configuration is environment, following the fleet
//! convention (`<SVC>_VAR`, systemd `Environment=` in production):
//!
//! - `KEEP_ADDR` — listen address. The unit binds OVH's tailnet address
//!   (`100.73.64.99:8106`); tailnet-only reachability is a bind decision,
//!   not firewall faith.
//! - `KEEP_DATA_DIR` — live `<name>.db` files. `/var/lib/keep` in
//!   production (the unit's `StateDirectory`), XDG data home locally.
//! - `KEEP_TOKENS_FILE` (required) — `name token` per line (`#` comments,
//!   blanks skipped), mode 600. Provisioning installs it; keep reads it
//!   once at startup, so token changes restart the service.
//! - `KEEP_SNAPSHOT_DIR` — `VACUUM INTO` staging (default
//!   `<data_dir>/snapshots`).
//! - `KEEP_SNAPSHOT_INTERVAL_SECS` — default 60.
//! - restic (`RESTIC_REPOSITORY`, `RESTIC_PASSWORD`, R2 credentials) comes
//!   from the environment via the unit's `EnvironmentFile`.

mod backup;
mod server;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use fleet_common::util::env_or;

#[derive(Parser)]
#[command(name = "keep", about = "The fleet's central database service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (the default with no subcommand).
    Serve,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        None | Some(Command::Serve) => {
            if let Err(e) = run_serve().await {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

async fn run_serve() -> anyhow::Result<()> {
    fleet_common::http::init_tracing("keep=info,tower_http=info");

    let data_dir = PathBuf::from(env_or(
        "KEEP_DATA_DIR",
        default_data_dir().to_string_lossy().into_owned(),
    ));
    let tokens_file = std::env::var("KEEP_TOKENS_FILE")
        .map_err(|_| anyhow::anyhow!("KEEP_TOKENS_FILE is required"))?;
    let snapshot_dir = PathBuf::from(env_or(
        "KEEP_SNAPSHOT_DIR",
        data_dir.join("snapshots").to_string_lossy().into_owned(),
    ));
    let interval_secs: u64 = env_or("KEEP_SNAPSHOT_INTERVAL_SECS", "60".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("bad KEEP_SNAPSHOT_INTERVAL_SECS: {e}"))?;

    let entries = read_tokens_file(&tokens_file)?;
    let registry = Arc::new(store::Registry::open(&data_dir, entries).await?);
    tracing::info!("serving {:?} on {}", registry.names(), data_dir.display());

    let backup_registry = Arc::clone(&registry);
    tokio::spawn(backup::snapshot_loop(backup_registry, backup::BackupConfig {
        snapshot_dir,
        interval_secs,
    }));

    let addr: SocketAddr = env_or("KEEP_ADDR", "127.0.0.1:8106").parse()?;
    let app = server::router(Arc::new(server::AppState { registry }));
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// `$XDG_DATA_HOME/keep` (`~/.local/share/keep`): fleet-common's
/// `default_db_path` returns a file path, but keep owns a directory of
/// databases, so the two lines live here instead of being bent to fit.
fn default_data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".local/share")
    });
    base.join("keep")
}

/// `name token` per line; `#` comments and blanks skipped. Malformed lines
/// fail startup loudly — a silently-skipped token line would provision a
/// database nobody can reach.
fn read_tokens_file(path: &str) -> anyhow::Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading tokens file {path:?}: {e}"))?;
    let mut entries = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some(name), Some(token), None) => {
                entries.push((name.to_owned(), token.to_owned()));
            }
            _ => anyhow::bail!("tokens file {path:?} line {}: want `name token`", lineno + 1),
        }
    }
    Ok(entries)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_file_parses_and_rejects() {
        let dir = std::env::temp_dir().join(format!("keep-tokens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens");
        std::fs::write(
            &path,
            "# comment\n\nrecipes abc123\nmirror def456  \n",
        )
        .unwrap();
        let entries = read_tokens_file(path.to_str().unwrap()).unwrap();
        assert_eq!(
            entries,
            vec![
                ("recipes".to_owned(), "abc123".to_owned()),
                ("mirror".to_owned(), "def456".to_owned()),
            ]
        );
        for bad in ["onlyname\n", "a b c\n", "   \n"] {
            std::fs::write(&path, bad).unwrap();
            // A blank file is valid (zero databases fail later, at open);
            // malformed lines fail here.
            if bad.trim().is_empty() {
                assert!(read_tokens_file(path.to_str().unwrap()).unwrap().is_empty());
            } else {
                assert!(
                    read_tokens_file(path.to_str().unwrap()).is_err(),
                    "{bad:?} should be rejected"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
