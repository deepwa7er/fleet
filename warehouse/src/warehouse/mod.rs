pub mod crawler;
pub mod db;
pub mod git_extract;
pub mod heuristic;
pub mod shell_extract;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRecord {
    pub repo_id: String,
    pub path: String,
    pub name: String,
    pub primary_language: Option<String>,
    pub languages: Vec<String>,
    pub build_system: Option<String>,
    pub test_cmd: Option<String>,
    pub deploy_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub repo_id: String,
    pub path: String,
    pub rel_path: String,
    pub language: Option<String>,
    pub ext: Option<String>,
    pub bytes: i64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyRecord {
    pub repo_id: String,
    pub dependency: String,
    pub version: Option<String>,
    pub source_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationRecord {
    pub src_repo_id: String,
    pub dst_repo_id: Option<String>,
    pub dst_name: String,
    pub kind: String,
    pub evidence: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRecord {
    pub repo_id: String,
    pub commit_hash: String,
    pub author: Option<String>,
    pub author_email: Option<String>,
    pub ts: chrono::DateTime<chrono::Utc>,
    pub message: Option<String>,
    pub files_changed: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellRecord {
    pub ts: Option<chrono::DateTime<chrono::Utc>>,
    pub repo_id: Option<String>,
    pub cwd: Option<String>,
    pub cmd: String,
    pub tool_name: Option<String>,
    pub raw_line: String,
}

pub fn hash_repo_id(path: &str) -> String {
    let h = blake3::hash(path.as_bytes());
    h.to_hex()[..16].to_string()
}

pub fn ext_to_language(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "go" => Some("go"),
        "rb" => Some("ruby"),
        "java" => Some("java"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "toml" => Some("toml"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "md" => Some("markdown"),
        "sql" => Some("sql"),
        _ => None,
    }
}

pub fn detect_build_system(files: &[String]) -> (Option<String>, Option<String>) {
    let has = |name: &str| files.iter().any(|f| f == name || f.ends_with(&format!("/{name}")));
    if has("Cargo.toml") {
        return (Some("cargo".to_string()), Some("cargo test".to_string()));
    }
    if has("package.json") {
        return (Some("npm".to_string()), Some("npm test".to_string()));
    }
    if has("go.mod") {
        return (Some("go".to_string()), Some("go test ./...".to_string()));
    }
    if has("pyproject.toml") || has("setup.py") || has("requirements.txt") {
        return (Some("python".to_string()), Some("pytest".to_string()));
    }
    if has("Makefile") {
        return (Some("make".to_string()), Some("make test".to_string()));
    }
    (None, None)
}

pub fn tool_from_cmd(cmd: &str) -> Option<String> {
    let first = cmd.split_whitespace().next()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    // normalize common wrappers
    let tool = match base {
        "cargo" | "rustc" | "rustup" => "cargo",
        "npm" | "npx" | "yarn" | "pnpm" | "bun" => "npm",
        "python" | "python3" | "pip" | "pip3" | "pytest" | "poetry" => "python",
        "go" => "go",
        "git" => "git",
        "docker" | "podman" => "docker",
        "make" | "cmake" => "make",
        _ => base,
    };
    Some(tool.to_string())
}
