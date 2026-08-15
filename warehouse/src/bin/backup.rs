use anyhow::Context;
use aws_config::BehaviorVersion;
use aws_sdk_s3::primitives::ByteStream;
use clap::Parser;
use warehouse::config::Config;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "warehouse-backup", about = "Disaster-recovery backup of SQLite + Parquet to R2 (S3-compatible). No-op if R2 env not set.")]
struct Args {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    full: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let cfg = Config::from_env()?;
    info!(has_r2 = cfg.has_r2(), dry_run = args.dry_run, "backup start");

    if !cfg.has_r2() {
        println!("R2 not configured. Set R2_ENDPOINT, R2_BUCKET, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY in .env");
        println!("Local warehouse remains at {} - backup skipped.", cfg.warehouse_db.display());
        println!("To test ingest without R2, just run warehouse-ingest.");
        return Ok(());
    }

    // checkpoint to ensure consistent file (SQLite WAL)
    {
        let conn = warehouse::warehouse::db::open_and_migrate(&cfg.warehouse_db)?;
        warehouse::warehouse::db::checkpoint(&conn)?;
    }

    let s3 = build_s3_client(&cfg).await?;

    let bucket = cfg.r2_bucket.clone().unwrap();
    let prefix = cfg.r2_prefix.clone();

    // collect local files to upload
    let mut files: Vec<(PathBuf, String)> = Vec::new(); // (local, remote_key)

    // warehouse db itself (now sqlite) - upload both live and versioned snapshot
    if cfg.warehouse_db.exists() {
        let key = format!("{prefix}warehouse/warehouse.sqlite");
        files.push((cfg.warehouse_db.clone(), key));
        // also upload WAL/SHM if exists for completeness
        let wal = cfg.warehouse_db.with_extension("sqlite-wal");
        if wal.exists() {
            let key_wal = format!("{prefix}warehouse/warehouse.sqlite-wal");
            files.push((wal, key_wal));
        }
        let shm = cfg.warehouse_db.with_extension("sqlite-shm");
        if shm.exists() {
            let key_shm = format!("{prefix}warehouse/warehouse.sqlite-shm");
            files.push((shm, key_shm));
        }
        // versioned copy
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let key2 = format!("{prefix}snapshots/warehouse-{ts}.sqlite");
        files.push((cfg.warehouse_db.clone(), key2));
    }
    // also support legacy duckdb path if user migrated
    let legacy = cfg.warehouse_dir.join("warehouse.duckdb");
    if legacy.exists() {
        let key = format!("{prefix}warehouse/warehouse.duckdb");
        files.push((legacy, key));
    }

    // raw parquet tree
    for entry in WalkDir::new(&cfg.warehouse_raw_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.path().is_dir() { continue; }
        let rel = entry.path().strip_prefix(&cfg.warehouse_dir).unwrap_or(entry.path());
        let key = format!("{prefix}{}", rel.display().to_string().replace('\\', "/"));
        files.push((entry.path().to_path_buf(), key));
    }

    info!(files = files.len(), bucket = %bucket, "uploading");

    for (local, key) in files {
        if args.dry_run {
            println!("would upload {} -> s3://{}/{}", local.display(), bucket, key);
            continue;
        }
        upload_file(&s3, &bucket, &key, &local).await.unwrap_or_else(|e| {
            warn!(key = %key, error = %e, "upload failed");
        });
    }

    if !args.dry_run {
        println!("Backup complete to s3://{}/{} (done)", bucket, prefix);
        println!("Restore: rclone sync r2:{}/{} ~/data/warehouse --checksum  OR  aws s3 sync s3://{}/{} ~/data/warehouse --endpoint-url $R2_ENDPOINT", bucket, prefix, bucket, prefix);
    }

    Ok(())
}

async fn build_s3_client(cfg: &Config) -> anyhow::Result<aws_sdk_s3::Client> {
    let endpoint = cfg.r2_endpoint.clone().unwrap();
    let region = cfg.r2_region.clone();
    let region_str = if region == "auto" { "auto" } else { region.as_str() };
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_config::Region::new(region_str.to_string()))
        .endpoint_url(endpoint)
        .load()
        .await;
    if let (Some(ak), Some(sk)) = (&cfg.r2_access_key_id, &cfg.r2_secret_access_key) {
        std::env::set_var("AWS_ACCESS_KEY_ID", ak);
        std::env::set_var("AWS_SECRET_ACCESS_KEY", sk);
    }
    Ok(aws_sdk_s3::Client::new(&config))
}

async fn upload_file(client: &aws_sdk_s3::Client, bucket: &str, key: &str, local: &Path) -> anyhow::Result<()> {
    let body = ByteStream::from_path(local).await.context("read local file")?;
    let hash = hash_file(local)?;
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(body)
        .metadata("blake3", hash)
        .send()
        .await
        .context("put_object")?;
    info!(key = %key, "uploaded");
    Ok(())
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
