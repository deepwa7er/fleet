//! Reading a repo's tracked source: the file list (`git ls-files`) and a single
//! file's contents, guarded so only tracked, in-tree, text files are ever served.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// Files larger than this are returned as metadata only (no body) — a source
/// browser has no use for a multi-megabyte generated/data file, and shipping it
/// would just bloat the response.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// How many leading bytes to sniff for a NUL when classifying binary content.
const SNIFF_BYTES: usize = 8192;

/// The tracked files in a repo, as repo-relative slash-separated paths, sorted.
/// `git ls-files` is the source of truth: it excludes `.gitignore`d files and
/// build output, so secrets and artifacts never appear.
pub async fn tracked_files(repo_dir: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("-z")
        .current_dir(repo_dir)
        .output()
        .await
        .with_context(|| format!("running git ls-files in {}", repo_dir.display()))?;
    if !output.status.success() {
        bail!(
            "git ls-files failed in {}: {}",
            repo_dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut files: Vec<String> = output
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    files.sort();
    Ok(files)
}

/// A single file's contents and metadata for the viewer.
pub struct FileContents {
    /// Repo-relative path, normalized to forward slashes.
    pub path: String,
    /// Extension-derived language hint for the frontend's syntax highlighter.
    pub language: String,
    /// Byte size of the file on disk.
    pub bytes: u64,
    /// Set when the file is binary (`content` is then `None`).
    pub binary: bool,
    /// Set when the file exceeded [`MAX_FILE_BYTES`] (`content` is then `None`).
    pub too_large: bool,
    /// The UTF-8 text, when the file is text and within the size cap.
    pub content: Option<String>,
}

/// Sanitize `rel` and confirm it names a tracked file in `repo_dir`, returning
/// the cleaned repo-relative path. Shared by reads and writes so both reach
/// exactly the same set of files — never a `.gitignore`d one (which may hold
/// secrets) and never a path that escapes the repo.
pub async fn ensure_tracked_path(repo_dir: &Path, rel: &str) -> Result<String> {
    let safe = sanitize_relpath(rel)?;
    // `--error-unmatch` exits non-zero for an untracked path.
    let tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch", "-z", "--"])
        .arg(&safe)
        .current_dir(repo_dir)
        .output()
        .await
        .with_context(|| format!("checking tracked status of {safe} in {}", repo_dir.display()))?;
    if !tracked.status.success() {
        bail!("not a tracked file: {safe}");
    }
    Ok(safe)
}

/// Read one tracked file. Rejects path traversal and untracked paths, so the
/// only things reachable are the same files [`tracked_files`] lists.
pub async fn read_file(repo_dir: &Path, rel: &str) -> Result<FileContents> {
    // Reading straight from the working tree keeps live edits visible.
    let safe = ensure_tracked_path(repo_dir, rel).await?;

    let full = repo_dir.join(&safe);
    let meta = tokio::fs::metadata(&full)
        .await
        .with_context(|| format!("stat {}", full.display()))?;
    if !meta.is_file() {
        bail!("not a regular file: {safe}");
    }
    let bytes = meta.len();
    let language = language_for(&safe);

    if bytes > MAX_FILE_BYTES {
        return Ok(FileContents {
            path: safe,
            language,
            bytes,
            binary: false,
            too_large: true,
            content: None,
        });
    }

    let raw = tokio::fs::read(&full).await.with_context(|| format!("reading {}", full.display()))?;
    if is_binary(&raw) {
        return Ok(FileContents {
            path: safe,
            language,
            bytes,
            binary: true,
            too_large: false,
            content: None,
        });
    }

    // Lossy is fine: we already ruled out binary; this only repairs the rare
    // invalid byte in otherwise-text source so the viewer always gets a string.
    let content = String::from_utf8_lossy(&raw).into_owned();
    Ok(FileContents {
        path: safe,
        language,
        bytes,
        binary: false,
        too_large: false,
        content: Some(content),
    })
}

/// Normalize and validate a repo-relative path: forward slashes, no absolute
/// paths, no `.` / `..` components. Returns the cleaned relative path, so a
/// request can never escape the repo directory.
fn sanitize_relpath(rel: &str) -> Result<String> {
    let rel = rel.replace('\\', "/");
    let path = Path::new(&rel);
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            // Reject anything that could climb out of the repo or anchor to root.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("illegal path: {rel}")
            }
            Component::CurDir => {}
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("empty path");
    }
    Ok(clean.to_string_lossy().replace('\\', "/"))
}

/// Treat content as binary if it contains a NUL in its first chunk — the same
/// cheap heuristic git and ripgrep use.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0)
}

/// Map a file path to a syntax-highlighting language id (the frontend passes
/// this straight to Shiki). Falls back to the bare extension, then `text`.
fn language_for(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A few extensionless files worth naming explicitly.
    match name {
        "Dockerfile" => return "docker".into(),
        "Makefile" => return "makefile".into(),
        _ => {}
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let lang = match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "sh" | "bash" | "fish" => "bash",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        "plist" | "xml" => "xml",
        "" => "text",
        other => other,
    };
    lang.to_string()
}
