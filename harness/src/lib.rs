//! harness — a minimal coding-agent loop for the Kimi Code subscription API.
//!
//! The crate is a library built around one agent loop with two frontends:
//!
//!   - the terminal REPL (`harness`; see src/repl.rs and src/main.rs)
//!   - the tailnet web UI (`harness serve`; see src/serve.rs)
//!
//! A frontend implements [agent::TurnIo] (output events in, mid-turn user
//! input out); the loop ([agent::Session::run_turn]) drives the model,
//! executes tool calls, and reports everything through that trait. Async on
//! tokio + reqwest: SSE streaming, cancellation, and mid-turn interjections
//! are all `select!` branches. No compaction, no sandbox — see README.md.

pub mod agent;
pub mod auth;
pub mod prompt;
pub mod tools;
pub mod usage;
pub mod util;

use std::io::IsTerminal;

/// Diagnostic log line (auth refreshes, retry notices). Dimmed on a terminal,
/// plain into log files and pipes (launchd, CI). This is not user-facing
/// output — that flows through [agent::TurnIo].
pub fn log(text: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("\x1b[2m{text}\x1b[0m");
    } else {
        eprintln!("{text}");
    }
}
