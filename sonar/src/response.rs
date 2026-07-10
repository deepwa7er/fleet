//! Reading the chatbot's reply out of the HTTP response.
//!
//! Two body shapes are handled, chosen by `Content-Type`: a streamed
//! `text/event-stream` of `data:` chunks, and a single JSON document. In both
//! cases the assistant's text lives at a provider-specific path, so a set of
//! common paths is tried, with an explicit JSON-pointer override for anything
//! the heuristics miss. The full body is always retained untouched for review.

use serde_json::Value;

/// JSON-pointer paths where a full reply commonly sits, tried in order.
const REPLY_PATHS: &[&str] = &[
    "/reply",
    "/choices/0/message/content",
    "/choices/0/text",
    "/message/content",
    "/message",
    "/content",
    "/text",
    "/response",
    "/output",
    "/data/0/content",
];

/// JSON-pointer paths where a per-chunk streaming delta commonly sits.
const DELTA_PATHS: &[&str] = &[
    "/choices/0/delta/content",
    "/delta",
    "/content",
    "/text",
    "/token",
    "/choices/0/text",
    "/message/content",
];

/// The assistant text extracted from a response body, plus how it was read.
pub struct Reply {
    pub text: String,
    pub streamed: bool,
}

/// Extract the reply. `content_type` decides the shape; the optional pointer
/// overrides the heuristic path for that shape.
pub fn extract(content_type: Option<&str>, body: &str, pointer: Option<&str>) -> Reply {
    let is_sse = content_type
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/event-stream"));

    if is_sse {
        Reply {
            text: extract_sse(body, pointer),
            streamed: true,
        }
    } else {
        Reply {
            text: extract_json(body, pointer),
            streamed: false,
        }
    }
}

fn extract_json(body: &str, pointer: Option<&str>) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        // Not JSON at all (an HTML error page, plain text) — hand back the body.
        return body.to_string();
    };

    if let Some(ptr) = pointer
        && let Some(found) = value.pointer(ptr)
    {
        return value_to_string(found);
    }

    for path in REPLY_PATHS {
        if let Some(v) = value.pointer(path)
            && let Some(s) = v.as_str()
        {
            return s.to_string();
        }
    }

    // Recognisably JSON but no known reply field — pretty-print the whole thing
    // rather than guess, so nothing is silently dropped.
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_string())
}

fn extract_sse(body: &str, pointer: Option<&str>) -> String {
    let mut out = String::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim_start();
        if data == "[DONE]" {
            break;
        }
        match serde_json::from_str::<Value>(data) {
            Ok(value) => out.push_str(&sse_piece(&value, pointer)),
            // A non-JSON data line (some servers stream raw text) is the piece.
            Err(_) => out.push_str(data),
        }
    }
    if out.is_empty() {
        // Nothing matched — likely a non-standard stream; keep the raw body.
        return body.to_string();
    }
    out
}

fn sse_piece(value: &Value, pointer: Option<&str>) -> String {
    if let Some(ptr) = pointer {
        return value.pointer(ptr).and_then(Value::as_str).unwrap_or("").to_string();
    }
    for path in DELTA_PATHS {
        if let Some(s) = value.pointer(path).and_then(Value::as_str) {
            return s.to_string();
        }
    }
    String::new()
}

fn value_to_string(value: &Value) -> String {
    match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_style_json() {
        let body = r#"{"choices":[{"message":{"content":"Hello there"}}]}"#;
        let r = extract(Some("application/json"), body, None);
        assert_eq!(r.text, "Hello there");
        assert!(!r.streamed);
    }

    #[test]
    fn simple_reply_field() {
        let r = extract(Some("application/json; charset=utf-8"), r#"{"reply":"hi"}"#, None);
        assert_eq!(r.text, "hi");
    }

    #[test]
    fn pointer_override() {
        let body = r#"{"data":{"answer":"forty-two"}}"#;
        let r = extract(Some("application/json"), body, Some("/data/answer"));
        assert_eq!(r.text, "forty-two");
    }

    #[test]
    fn sse_stream_concatenates_deltas() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\
                    data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\
                    data: [DONE]\n";
        let r = extract(Some("text/event-stream"), body, None);
        assert_eq!(r.text, "Hello");
        assert!(r.streamed);
    }

    #[test]
    fn sse_raw_text_lines() {
        let body = "data: Hel\ndata: lo\n";
        let r = extract(Some("text/event-stream"), body, None);
        assert_eq!(r.text, "Hello");
    }

    #[test]
    fn non_json_body_passes_through() {
        let r = extract(Some("text/html"), "<h1>403 Forbidden</h1>", None);
        assert_eq!(r.text, "<h1>403 Forbidden</h1>");
    }
}
