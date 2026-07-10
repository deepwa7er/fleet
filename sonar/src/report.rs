//! The record of each prompt's outcome, and how it is presented.

use serde::Serialize;

/// Everything observed for a single prompt. The full response body is kept so
/// the results file is a complete audit record; the console shows a digest.
#[derive(Debug, Serialize)]
pub struct Outcome {
    pub index: usize,
    pub prompt: String,
    /// HTTP status, absent only when the request never got a response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub latency_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Whether the body was read as a server-sent event stream.
    pub streamed: bool,
    /// Best-effort extracted assistant text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// The untouched response body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A transport-level failure (timeout, connection refused, TLS error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The whole run, for the JSON results file.
#[derive(Debug, Serialize)]
pub struct Report {
    pub target: String,
    pub started_at: String,
    pub prompt_count: usize,
    pub outcomes: Vec<Outcome>,
}

const SNIPPET_LEN: usize = 160;

impl Outcome {
    /// One compact block per prompt for the terminal.
    pub fn render_console(&self) -> String {
        let status = match (self.status, &self.error) {
            (Some(code), _) => code.to_string(),
            (None, Some(_)) => "ERR".to_string(),
            (None, None) => "—".to_string(),
        };
        let mut block = format!(
            "#{:<3} {:>4}  {:>6}ms   {}\n     prompt: {}",
            self.index,
            status,
            self.latency_ms,
            if self.streamed { "(stream)" } else { "" },
            snippet(&self.prompt),
        );
        match (&self.reply, &self.error) {
            (_, Some(err)) => block.push_str(&format!("\n     error : {err}")),
            (Some(reply), None) => block.push_str(&format!("\n     reply : {}", snippet(reply))),
            (None, None) => {}
        }
        block
    }
}

/// Collapse whitespace and clip to a single readable line.
fn snippet(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars: Vec<char> = flat.chars().collect();
    if chars.len() > SNIPPET_LEN {
        chars.truncate(SNIPPET_LEN);
        format!("{}…", chars.into_iter().collect::<String>())
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_flattens_and_clips() {
        let s = snippet("line one\n  line   two");
        assert_eq!(s, "line one line two");
        let long = "x".repeat(SNIPPET_LEN + 50);
        assert!(snippet(&long).ends_with('…'));
    }
}
