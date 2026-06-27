//! The worker-facing CLI. Every command is a thin client over the local
//! server's HTTP API, so the server stays the single writer to SQLite.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

pub struct Client {
    base: String,
    agent: ureq::Agent,
}

pub enum CliError {
    /// The server isn't reachable (probably not running).
    Unreachable(String),
    /// The server returned a non-2xx status with an error message.
    Status(u16, String),
    /// Malformed response or other client-side failure.
    Other(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            // A claim conflict (409) is an expected "someone else got it" signal.
            CliError::Status(409, _) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Unreachable(msg) => {
                write!(f, "cannot reach drydock server (is `drydock serve` running?): {msg}")
            }
            CliError::Status(code, msg) => write!(f, "server returned {code}: {msg}"),
            CliError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl Client {
    pub fn from_env() -> Self {
        // Full base URL so the worker can point at the VPS through breakwater,
        // e.g. DRYDOCK_URL=https://drydock.internal.deepwa7er.com. Defaults to a
        // local server for development on the same machine.
        let base = std::env::var("DRYDOCK_URL").unwrap_or_else(|_| "http://127.0.0.1:8093".into());
        // Bounded timeouts so a hung or wedged server can't freeze the worker's
        // poll loop indefinitely — without them ureq waits forever on read.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .build();
        Client {
            base: base.trim_end_matches('/').to_string(),
            agent,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub fn get(&self, path: &str) -> Result<Value, CliError> {
        Self::handle(self.agent.get(&self.url(path)).call())
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, CliError> {
        Self::handle(self.agent.post(&self.url(path)).send_json(body))
    }

    fn handle(result: Result<ureq::Response, ureq::Error>) -> Result<Value, CliError> {
        match result {
            Ok(resp) => resp.into_json().map_err(|e| CliError::Other(e.to_string())),
            Err(ureq::Error::Status(code, resp)) => Err(CliError::Status(code, extract_error(resp))),
            Err(ureq::Error::Transport(t)) => Err(CliError::Unreachable(t.to_string())),
        }
    }
}

fn extract_error(resp: ureq::Response) -> String {
    resp.into_json::<Value>()
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "request failed".into())
}

// ---- human-readable output ------------------------------------------------

fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn summary(v: &Value) -> String {
    format!(
        "#{} [{}] {} → {} ({})",
        v.get("id").and_then(Value::as_i64).unwrap_or(0),
        field(v, "priority"),
        field(v, "title"),
        field(v, "target"),
        field(v, "state"),
    )
}

pub fn print_next(v: &Value, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    } else if v.is_null() {
        println!("No open tickets.");
    } else {
        println!("{}", summary(v));
    }
}

pub fn print_one(v: &Value, action: &str) {
    println!(
        "#{} {} → state {}",
        v.get("id").and_then(Value::as_i64).unwrap_or(0),
        action,
        field(v, "state"),
    );
}

pub fn print_show(v: &Value, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        return;
    }
    println!("{}\n", summary(v));
    if let Some(branch) = v.get("branch").and_then(Value::as_str) {
        println!("branch: {branch}");
    }
    if let Some(pr) = v.get("pr_url").and_then(Value::as_str) {
        println!("pr:     {pr}");
    }
    println!("\n## Goal\n{}", field(v, "goal"));
    if let Some(a) = v.get("acceptance").and_then(Value::as_str) {
        println!("\n## Acceptance\n{a}");
    }
    if let Some(c) = v.get("constraints").and_then(Value::as_str) {
        println!("\n## Constraints\n{c}");
    }
    if let Some(comments) = v.get("comments").and_then(Value::as_array) {
        if !comments.is_empty() {
            println!("\n## Thread");
            for c in comments {
                println!(
                    "\n[{}] {} ({}):\n{}",
                    field(c, "created_at"),
                    field(c, "author"),
                    field(c, "kind"),
                    field(c, "body"),
                );
            }
        }
    }
}
