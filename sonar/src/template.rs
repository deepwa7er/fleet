//! The request template: a raw HTTP request captured from the proxy, with a
//! marker where each prompt is spliced in.
//!
//! The workflow mirrors Burp Intruder. You save a request out of the proxy
//! (right-click → Copy to file, or Repeater's "Save item"), drop a marker
//! such as `§PROMPT§` wherever the user message goes, and sonar substitutes
//! each prompt into a fresh copy for every run. The marker is position-blind:
//! it can sit in the JSON body, a header, or the request line.

use anyhow::{Context, Result, bail};

/// How a prompt is encoded before it replaces the marker. The default is
/// `Json` because the marker almost always lands inside a JSON string literal
/// in a chat API body, where an unescaped quote or newline would corrupt the
/// request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escape {
    /// Escape as a JSON string body (quotes, backslashes, control chars) but
    /// without the surrounding quotes — the template already supplies those.
    Json,
    /// Percent-encode everything outside the unreserved set, for a marker in a
    /// URL query or an `application/x-www-form-urlencoded` body.
    Url,
    /// Splice the prompt in verbatim.
    None,
}

impl std::str::FromStr for Escape {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "json" => Ok(Escape::Json),
            "url" => Ok(Escape::Url),
            "none" => Ok(Escape::None),
            other => bail!("unknown escape mode {other:?} (expected json, url, or none)"),
        }
    }
}

/// A parsed raw request template, ready to fill with prompts.
#[derive(Debug)]
pub struct Template {
    raw: String,
    marker: String,
    scheme: String,
}

impl Template {
    /// Parse a raw HTTP request. The scheme is not present in a saved request,
    /// so it is supplied separately (defaulting to https for chat endpoints).
    pub fn new(raw: String, marker: String, scheme: String) -> Result<Self> {
        if !raw.contains(&marker) {
            bail!(
                "marker {marker:?} does not appear anywhere in the request template; \
                 nothing would be substituted"
            );
        }
        Ok(Self { raw, marker, scheme })
    }

    /// Produce the concrete request for one prompt: escape it, splice it in
    /// wherever the marker appears, then parse the result into its parts.
    pub fn fill(&self, prompt: &str, escape: Escape) -> Result<FilledRequest> {
        let encoded = encode(prompt, escape);
        let filled = self.raw.replace(&self.marker, &encoded);
        parse_request(&filled, &self.scheme)
    }
}

/// The pieces of a concrete HTTP request, with the proxy-managed headers
/// already stripped so the client recomputes them from the final body.
#[derive(Debug)]
pub struct FilledRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Headers the HTTP client owns; carrying them over from the template would
/// desync the wire (a stale Content-Length after substitution) or duplicate
/// what the client sets from the URL (Host).
const MANAGED_HEADERS: &[&str] = &[
    "content-length",
    "host",
    "accept-encoding",
    "connection",
    "proxy-connection",
    "transfer-encoding",
];

fn parse_request(raw: &str, scheme: &str) -> Result<FilledRequest> {
    // Head and body are separated by the first blank line. Accept either CRLF
    // (how the proxy saves it) or bare LF (how a hand-edited file often ends
    // up), and normalise on the head only — the body is kept byte-for-byte.
    let (head, body) = split_head_body(raw);

    let mut lines = head.lines();
    let request_line = lines
        .next()
        .context("request template is empty")?
        .trim_end();
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .context("request line has no method")?
        .to_string();
    let target = parts
        .next()
        .context("request line has no request-target")?
        .to_string();

    let mut headers = Vec::new();
    let mut host = None;
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .with_context(|| format!("malformed header line: {line:?}"))?;
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_string());
        }
        if MANAGED_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) {
            continue;
        }
        headers.push((name.to_string(), value.to_string()));
    }

    let url = build_url(scheme, host.as_deref(), &target)?;

    Ok(FilledRequest {
        method,
        url,
        headers,
        body: body.to_string(),
    })
}

fn split_head_body(raw: &str) -> (&str, &str) {
    if let Some(idx) = raw.find("\r\n\r\n") {
        (&raw[..idx], &raw[idx + 4..])
    } else if let Some(idx) = raw.find("\n\n") {
        (&raw[..idx], &raw[idx + 2..])
    } else {
        (raw, "")
    }
}

fn build_url(scheme: &str, host: Option<&str>, target: &str) -> Result<String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(target.to_string());
    }
    let host = host.context(
        "request template has no Host header and a relative request-target, so the \
         URL cannot be built; add a Host header or use an absolute request-target",
    )?;
    Ok(format!("{scheme}://{host}{target}"))
}

fn encode(prompt: &str, escape: Escape) -> String {
    match escape {
        Escape::None => prompt.to_string(),
        Escape::Url => url_encode(prompt),
        // serde_json quotes and escapes the string; strip the surrounding
        // quotes because the template already has them around the marker.
        Escape::Json => {
            let quoted = serde_json::to_string(prompt)
                .expect("serialising a string to JSON is infallible");
            quoted[1..quoted.len() - 1].to_string()
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpl(raw: &str) -> Template {
        Template::new(raw.to_string(), "§PROMPT§".to_string(), "https".to_string()).unwrap()
    }

    #[test]
    fn json_escape_keeps_body_valid() {
        let t = tmpl("POST /c HTTP/1.1\r\nHost: x.test\r\n\r\n{\"m\":\"§PROMPT§\"}");
        let f = t.fill("he said \"hi\"\nbye", Escape::Json).unwrap();
        // The filled body must still parse as JSON.
        let v: serde_json::Value = serde_json::from_str(&f.body).unwrap();
        assert_eq!(v["m"], "he said \"hi\"\nbye");
        assert_eq!(f.url, "https://x.test/c");
        assert_eq!(f.method, "POST");
    }

    #[test]
    fn managed_headers_are_dropped() {
        let t = tmpl(
            "POST /c HTTP/1.1\r\nHost: x.test\r\nContent-Length: 99\r\n\
             Authorization: Bearer t\r\n\r\n§PROMPT§",
        );
        let f = t.fill("hello", Escape::None).unwrap();
        let names: Vec<_> = f.headers.iter().map(|(n, _)| n.to_lowercase()).collect();
        assert!(!names.contains(&"content-length".to_string()));
        assert!(!names.contains(&"host".to_string()));
        assert!(names.contains(&"authorization".to_string()));
    }

    #[test]
    fn absolute_target_wins_over_host() {
        let t = tmpl("GET https://real.test/p HTTP/1.1\r\nHost: proxy.test\r\n\r\n§PROMPT§");
        let f = t.fill("x", Escape::None).unwrap();
        assert_eq!(f.url, "https://real.test/p");
    }

    #[test]
    fn missing_marker_is_rejected() {
        let err = Template::new(
            "GET / HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
            "§PROMPT§".to_string(),
            "https".to_string(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn url_encode_escapes_reserved() {
        assert_eq!(url_encode("a b&c"), "a%20b%26c");
    }
}
