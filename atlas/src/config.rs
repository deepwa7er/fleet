//! atlas.toml: bind address, the projects to index, and tool/paths.
//!
//! Like source, atlas is a dev-box service — the checked-in atlas.toml holds
//! the production values (tailnet bind) and local development overrides them
//! with a loopback copy.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    /// SQLite path; defaults to the XDG data dir when absent.
    #[serde(default)]
    pub db: Option<PathBuf>,
    pub web_dir: PathBuf,
    /// The rust-analyzer binary that produces the SCIP index.
    #[serde(default = "default_rust_analyzer")]
    pub rust_analyzer: String,
    #[serde(rename = "project", default)]
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub name: String,
    /// The cargo workspace (or single-crate) root to index.
    pub path: PathBuf,
}

fn default_rust_analyzer() -> String {
    "rust-analyzer".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;

        if config.projects.is_empty() {
            bail!("config lists no [[project]] entries — nothing to index");
        }
        let mut names: Vec<&str> = config.projects.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() != config.projects.len() {
            bail!("config lists duplicate project names");
        }

        config.db = config.db.map(|p| expand_tilde(&p));
        config.web_dir = expand_tilde(&config.web_dir);
        for project in &mut config.projects {
            project.path = expand_tilde(&project.path);
        }
        Ok(config)
    }

    pub fn db_path(&self) -> PathBuf {
        self.db
            .clone()
            .unwrap_or_else(|| fleet_common::util::default_db_path("atlas", "atlas.db"))
    }

    pub fn project(&self, name: &str) -> Option<&ProjectConfig> {
        self.projects.iter().find(|p| p.name == name)
    }
}

/// `~/x` → `$HOME/x`. Only the leading tilde form; `~user` is not supported.
fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_config_and_expands_tildes() {
        let dir = std::env::temp_dir().join(format!("atlas-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("atlas.toml");
        std::fs::write(
            &path,
            r#"
bind = "127.0.0.1"
port = 7880
web_dir = "~/code/fleet/atlas/web/dist"

[[project]]
name = "fleet"
path = "~/code/fleet"
"#,
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.port, 7880);
        assert_eq!(config.rust_analyzer, "rust-analyzer");
        assert!(!config.web_dir.to_string_lossy().contains('~'));
        assert_eq!(config.projects.len(), 1);
        assert!(config.project("fleet").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_duplicate_project_names() {
        let dir = std::env::temp_dir().join(format!("atlas-config-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("atlas.toml");
        std::fs::write(
            &path,
            r#"
bind = "127.0.0.1"
port = 7880
web_dir = "web/dist"

[[project]]
name = "fleet"
path = "/a"

[[project]]
name = "fleet"
path = "/b"
"#,
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "got: {err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
