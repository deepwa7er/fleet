use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

/// Placeholder in URL templates that gets replaced with the
/// percent-encoded remainder of the address-bar input.
pub const QUERY_PLACEHOLDER: &str = "{query}";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Port the server listens on (loopback only). Changing it requires a restart.
    #[serde(default = "default_port")]
    pub port: u16,

    /// URL template used when the input matches no command.
    /// Must contain `{query}`.
    pub fallback: String,

    /// Command name -> URL or URL template containing `{query}`.
    /// BTreeMap so the /commands page lists them alphabetically.
    #[serde(default)]
    pub commands: BTreeMap<String, String>,

    /// Optional note-capture command: `<keyword> <text>` POSTs the text to a
    /// notes app instead of redirecting. Omit the section to disable it.
    #[serde(default)]
    pub capture: Option<CaptureConfig>,
}

/// Turns one keyword into a capture action: `b lg buy milk` POSTs `{"text":
/// "buy milk"}` to `api` and confirms; `b lg` with no text just opens `open`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    /// Address-bar keyword that triggers capture, e.g. `lg`.
    pub keyword: String,
    /// Endpoint to POST `{"text": ...}` to — the notes app's capture API
    /// (loopback on the VPS, so no TLS hop).
    pub api: String,
    /// The notes app's web UI: linked from the confirmation page, and where a
    /// bare keyword (no text) redirects to.
    pub open: String,
}

fn default_port() -> u16 {
    7777
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Config::from_toml(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Config> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if !self.fallback.contains(QUERY_PLACEHOLDER) {
            bail!("`fallback` must contain {QUERY_PLACEHOLDER}");
        }
        if !is_absolute_url(&self.fallback) {
            bail!("`fallback` must be an absolute URL (scheme://...)");
        }
        for (name, url) in &self.commands {
            validate_command_name(name).map_err(anyhow::Error::msg)?;
            validate_command_url(url).map_err(anyhow::Error::msg)?;
        }
        if let Some(capture) = &self.capture {
            validate_command_name(&capture.keyword).map_err(anyhow::Error::msg)?;
            // `api` and `open` are absolute URLs, same rule as a command target.
            validate_command_url(&capture.api).map_err(anyhow::Error::msg)?;
            validate_command_url(&capture.open).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}

/// Whether a command name is usable as the first address-bar token.
/// Returns a user-facing message on rejection so the same rule can back both
/// config loading and the add-command UI.
pub fn validate_command_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Command name is required.".to_string());
    }
    if name.chars().any(char::is_whitespace) {
        return Err(format!("Command name {name:?} must be a single word (no spaces)."));
    }
    Ok(())
}

/// Whether a command target is usable as a redirect destination. It must be an
/// absolute URL: a relative one would resolve against ferry's own origin rather
/// than sending the browser where intended.
pub fn validate_command_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL is required.".to_string());
    }
    if !is_absolute_url(url) {
        return Err(format!("URL {url:?} must be absolute, e.g. https://example.com/"));
    }
    Ok(())
}

/// An absolute URL here means `scheme://rest` with a syntactically valid scheme.
/// This is intentionally a structural check, not a full URL parse: ferry only
/// needs to know the value won't be treated as relative by the browser.
pub fn is_absolute_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    !rest.is_empty()
        && !scheme.is_empty()
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let config =
            Config::from_toml(r#"fallback = "https://example.com/search?q={query}""#).unwrap();
        assert_eq!(config.port, 7777);
        assert!(config.commands.is_empty());
    }

    #[test]
    fn rejects_fallback_without_placeholder() {
        assert!(Config::from_toml(r#"fallback = "https://example.com/""#).is_err());
    }

    #[test]
    fn rejects_command_name_with_whitespace() {
        let err = Config::from_toml(
            r#"
            fallback = "https://example.com/search?q={query}"
            [commands]
            "two words" = "https://example.com/"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_unknown_top_level_keys() {
        let err = Config::from_toml(
            r#"
            fallback = "https://example.com/search?q={query}"
            falback_typo = "oops"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_relative_command_url() {
        let err = Config::from_toml(
            r#"
            fallback = "https://example.com/search?q={query}"
            [commands]
            mail = "mail.example.com"
            "#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn rejects_relative_fallback() {
        assert!(Config::from_toml(r#"fallback = "/search?q={query}""#).is_err());
    }

    #[test]
    fn absolute_url_check() {
        assert!(is_absolute_url("https://example.com/"));
        assert!(is_absolute_url("http://localhost:7777/?q={query}"));
        assert!(!is_absolute_url("example.com"));
        assert!(!is_absolute_url("/relative"));
        assert!(!is_absolute_url("://nohost"));
        assert!(!is_absolute_url("https://"));
        assert!(!is_absolute_url("1nvalid://example.com"));
    }
}
