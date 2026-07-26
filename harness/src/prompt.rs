//! The system prompt: a base from ~/.config/harness/system.md (seeded with
//! the default on first run), plus — in order — AGENTS.md from the session's
//! working directory, $KIMI_SYSTEM_PROMPT, --system-file, --system.
//!
//! Placeholders {cwd}, {os}, {date} are substituted at load; HTML comments
//! (`<!-- ... -->`) are stripped before the prompt is sent.

use crate::agent::SessionConfig;
use crate::util::{home, today};
use std::path::{Path, PathBuf};

/// Fallback base prompt, and the body of the seeded system.md.
/// Placeholders {cwd}, {os}, {date} are substituted at load time.
const DEFAULT_SYSTEM_PROMPT: &str = "You are a coding agent running in a terminal. \
Working directory: {cwd}. OS: {os}. Date: {date}. \
Keep changes minimal and verify them by running code or tests when possible. \
Answer concisely.\n\
\n\
Your tools:\n\
- read_file, write_file, edit_file — read and modify files. edit_file replaces \
a unique exact string; read a file before editing it.\n\
- search — ripgrep-powered content search (regex, optional path/glob/ignore_case). \
Prefer it over shelling out to grep/find.\n\
- run_command — run a shell command and get stdout+stderr and the exit code. \
Commands time out after 120s and long output is truncated.\n\
- ask_user — ask the user a clarifying question and receive their typed answer \
as the tool result. If the request is ambiguous or a decision is needed, ask \
rather than guess.\n\
\n\
The user may type guidance while you work; if tool results say \"error: \
interrupted by user\" and a new user message follows, treat it as a change of \
direction, not a failure.";

const SYSTEM_FILE_COMMENT: &str = "<!-- Base system prompt for harness. Edit freely.
Placeholders: {cwd} = working directory, {os} = OS, {date} = today (YYYY-MM-DD).
Appended after this file, in order: ./AGENTS.md, $KIMI_SYSTEM_PROMPT, --system-file, --system.
HTML comments like this one are stripped before the prompt is sent. -->";

fn system_file_path() -> PathBuf {
    home().join(".config").join("harness").join("system.md")
}

/// The base system prompt, loaded from ~/.config/harness/system.md.
/// The file is seeded with the default on first run so it can be tweaked.
fn base_system_prompt(cwd: &Path) -> String {
    let path = system_file_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let seed = format!("{SYSTEM_FILE_COMMENT}\n\n{DEFAULT_SYSTEM_PROMPT}");
        match std::fs::write(&path, seed) {
            Ok(()) => crate::log(&format!(
                "created {} — edit it to customize the base system prompt",
                path.display()
            )),
            Err(e) => crate::log(&format!("could not create {}: {e}", path.display())),
        }
    }
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_SYSTEM_PROMPT.to_string());
    let stripped = strip_html_comments(&raw);
    let template = stripped.trim();
    let template = if template.is_empty() { DEFAULT_SYSTEM_PROMPT } else { template };
    template
        .replace("{cwd}", &cwd.display().to_string())
        .replace("{os}", "macOS")
        .replace("{date}", &today())
}

/// Remove `<!-- ... -->` blocks (multi-line; an unterminated one eats the rest).
fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The full system prompt for a session: the base from
/// ~/.config/harness/system.md, plus (in order) AGENTS.md from the session
/// cwd, $KIMI_SYSTEM_PROMPT, the config's --system-file, and --system text.
pub fn build_system_prompt(config: &SessionConfig) -> Result<String, String> {
    let mut prompt = base_system_prompt(&config.cwd);
    if let Ok(project) = std::fs::read_to_string(config.cwd.join("AGENTS.md"))
        && !project.trim().is_empty() {
            prompt.push_str("\n\nProject instructions from AGENTS.md:\n");
            prompt.push_str(project.trim());
        }
    if let Ok(extra) = std::env::var("KIMI_SYSTEM_PROMPT")
        && !extra.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(extra.trim());
        }
    if let Some(path) = &config.system_file {
        match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => {
                prompt.push_str("\n\n");
                prompt.push_str(text.trim());
            }
            Ok(_) => {}
            Err(e) => return Err(format!("--system-file {}: {e}", path.display())),
        }
    }
    if let Some(extra) = &config.system_extra
        && !extra.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(extra.trim());
        }
    Ok(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_html_comments() {
        assert_eq!(strip_html_comments("a <!-- x --> b"), "a  b");
        assert_eq!(strip_html_comments("a <!-- multi\nline --> b"), "a  b");
        assert_eq!(strip_html_comments("a <!-- unterminated"), "a ");
        assert_eq!(strip_html_comments("no comments"), "no comments");
    }
}
