//! Bridge credentials, resolved the way the bridge's other consumers do it:
//! `SKIFF_BRIDGE_PASSWORD` from the environment wins (tests, one-offs),
//! otherwise the skiff secrets file — `KEY=VALUE` lines taken verbatim,
//! `#` comments and blanks skipped, never shell-expanded — at
//! `$SKIFF_SECRETS_FILE` or `~/.config/skiff/secrets`. The password is
//! never printed and never reaches an error message.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub fn bridge_password() -> Result<String> {
    if let Ok(password) = std::env::var("SKIFF_BRIDGE_PASSWORD") {
        if !password.is_empty() {
            return Ok(password);
        }
    }
    let path = secrets_file();
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read the skiff secrets file at {}", path.display()))?;
    match parse(&raw, "SKIFF_BRIDGE_PASSWORD") {
        Some(password) => Ok(password),
        None => bail!(
            "SKIFF_BRIDGE_PASSWORD is not set and {} does not define it",
            path.display()
        ),
    }
}

fn secrets_file() -> PathBuf {
    if let Ok(path) = std::env::var("SKIFF_SECRETS_FILE") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    home().join(".config").join("skiff").join("secrets")
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is not set"))
}

fn parse(raw: &str, key: &str) -> Option<String> {
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = line.split_once('=') {
            if name == key && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_verbatim_and_skips_comments() {
        let raw = "# comment\n\nOTHER=x\nSKIFF_BRIDGE_PASSWORD=p$ss word=1\n";
        assert_eq!(parse(raw, "SKIFF_BRIDGE_PASSWORD").as_deref(), Some("p$ss word=1"));
        assert_eq!(parse(raw, "MISSING"), None);
    }
}
