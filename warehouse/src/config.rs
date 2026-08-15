use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub warehouse_dir: PathBuf,
    pub warehouse_db: PathBuf,
    pub warehouse_raw_dir: PathBuf,
    pub code_root: PathBuf,
    pub shell_history: PathBuf,
    pub git_commits_per_repo: usize,
    pub r2_endpoint: Option<String>,
    pub r2_bucket: Option<String>,
    pub r2_region: String,
    pub r2_access_key_id: Option<String>,
    pub r2_secret_access_key: Option<String>,
    pub r2_prefix: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let warehouse_dir = env_path("WAREHOUSE_DIR", "/home/deepwater/data/warehouse");
        let warehouse_db = env_path(
            "WAREHOUSE_DB",
            &format!("{}/warehouse.sqlite", warehouse_dir.display()),
        );
        let warehouse_raw_dir = env_path(
            "WAREHOUSE_RAW_DIR",
            &format!("{}/raw", warehouse_dir.display()),
        );
        Ok(Self {
            warehouse_dir: warehouse_dir.clone(),
            warehouse_db,
            warehouse_raw_dir,
            code_root: env_path("CODE_ROOT", "/home/deepwater/code"),
            shell_history: env_path("SHELL_HISTORY", "/home/deepwater/.bash_history"),
            git_commits_per_repo: std::env::var("INGEST_GIT_COMMITS_PER_REPO")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            r2_endpoint: std::env::var("R2_ENDPOINT").ok().filter(|v| !v.is_empty() && !v.contains("<accountid>")),
            r2_bucket: std::env::var("R2_BUCKET").ok().filter(|v| !v.is_empty()),
            r2_region: std::env::var("R2_REGION").unwrap_or_else(|_| "auto".to_string()),
            r2_access_key_id: std::env::var("R2_ACCESS_KEY_ID").ok().filter(|v| !v.is_empty()),
            r2_secret_access_key: std::env::var("R2_SECRET_ACCESS_KEY").ok().filter(|v| !v.is_empty()),
            r2_prefix: std::env::var("R2_PREFIX").unwrap_or_else(|_| "warehouse/".to_string()),
        })
    }

    pub fn has_r2(&self) -> bool {
        self.r2_endpoint.is_some()
            && self.r2_bucket.is_some()
            && self.r2_access_key_id.is_some()
            && self.r2_secret_access_key.is_some()
    }
}

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var(key)
        .unwrap_or_else(|_| default.to_string())
        .into()
}
