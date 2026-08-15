use super::{ShellRecord, tool_from_cmd};
use std::collections::HashMap;
use std::path::Path;

pub fn extract_shell(
    history_path: &Path,
    code_root: &Path,
    repo_name_to_id: &HashMap<String, String>,
    repo_path_to_id: &HashMap<String, String>,
) -> anyhow::Result<Vec<ShellRecord>> {
    let content = match std::fs::read_to_string(history_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let raw = line.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        // bash_history may have timestamps like ": 123456:0;cmd" — strip
        let cmd = strip_bash_timestamp(raw);
        if cmd.is_empty() {
            continue;
        }
        let tool_name = tool_from_cmd(&cmd);
        // heuristic repo association: if cmd contains a path under code_root or repo name
        let repo_id = infer_repo(&cmd, code_root, repo_name_to_id, repo_path_to_id);
        out.push(ShellRecord {
            ts: None,
            repo_id,
            cwd: None,
            cmd: truncate(&cmd, 1000),
            tool_name,
            raw_line: truncate(raw, 1000),
        });
        if out.len() > 20_000 {
            break;
        }
    }
    Ok(out)
}

fn strip_bash_timestamp(s: &str) -> String {
    if s.starts_with(": ") {
        if let Some(idx) = s.find(';') {
            return s[idx + 1..].trim().to_string();
        }
    }
    // zsh extended history: ": 1710000000:0;cmd"
    if s.starts_with(":") && s.contains(';') {
        if let Some(idx) = s.find(';') {
            return s[idx + 1..].trim().to_string();
        }
    }
    s.to_string()
}

fn infer_repo(
    cmd: &str,
    code_root: &Path,
    repo_name_to_id: &HashMap<String, String>,
    repo_path_to_id: &HashMap<String, String>,
) -> Option<String> {
    let lower = cmd.to_ascii_lowercase();
    // direct path match
    for (path, id) in repo_path_to_id {
        if lower.contains(&path.to_ascii_lowercase()) {
            return Some(id.clone());
        }
    }
    for (name, id) in repo_name_to_id {
        if contains_word(&lower, &name.to_ascii_lowercase()) {
            return Some(id.clone());
        }
    }
    // code_root itself
    if lower.contains(&code_root.display().to_string().to_ascii_lowercase()) {
        // map to code_root hash if present
        return None;
    }
    None
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.split(|c: char| !c.is_alphanumeric()).any(|w| w == needle)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}
