//! Loading the list of prompts to fire.
//!
//! Two shapes are accepted, chosen by file extension. A `.json` file is a JSON
//! array of strings, which is the robust choice when prompts span multiple
//! lines or carry awkward characters. Anything else is treated as a wordlist:
//! one prompt per line, blank lines and `#` comments ignored — the familiar
//! shape for a quick set of one-liners.

use std::path::Path;

use anyhow::{Context, Result, bail};

pub fn load(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading prompts from {}", path.display()))?;

    let prompts = if path.extension().is_some_and(|e| e == "json") {
        parse_json(&text)?
    } else {
        parse_lines(&text)
    };

    if prompts.is_empty() {
        bail!("no prompts found in {}", path.display());
    }
    Ok(prompts)
}

fn parse_json(text: &str) -> Result<Vec<String>> {
    let value: serde_json::Value =
        serde_json::from_str(text).context("parsing prompts file as JSON")?;
    let array = value
        .as_array()
        .context("a .json prompts file must be an array of strings")?;
    array
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .context("every element of the prompts array must be a string")
        })
        .collect()
}

fn parse_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_skip_blanks_and_comments() {
        let got = parse_lines("one\n\n# a comment\n  two  \n");
        assert_eq!(got, vec!["one", "two"]);
    }

    #[test]
    fn json_array_of_strings() {
        let got = parse_json("[\"a\", \"multi\\nline\"]").unwrap();
        assert_eq!(got, vec!["a", "multi\nline"]);
    }

    #[test]
    fn json_rejects_non_strings() {
        assert!(parse_json("[1, 2]").is_err());
    }
}
