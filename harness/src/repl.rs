//! The terminal frontend: a REPL (or one-shot --prompt) around the agent loop.
//!
//! [TerminalIo] renders the loop's events exactly as harness always has —
//! reasoning dimmed to stderr, answer text through the streaming markdown
//! renderer, a spinner covering time-to-first-token — and sources mid-turn
//! input from stdin: a typed line while the model works is an interjection
//! (the in-flight request is cancelled, the line joins the history, and the
//! turn continues), Ctrl-C aborts the turn, a second Ctrl-C force-quits.

use clap::Args;
use harness::agent::{Session, SessionConfig, Steer, TurnIo, cancelled};
use harness::usage;
use harness::util::{est_tokens, fmt_num, message_chars};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, Lines};

use crate::render::StreamRenderer;

#[derive(Args)]
pub struct ReplArgs {
    /// One-shot prompt (no REPL)
    #[arg(short, long)]
    pub prompt: Option<String>,
    /// Confirm each run_command before running it (yolo mode is the default)
    #[arg(long = "no-yolo")]
    pub no_yolo: bool,
    /// Model id (default: $KIMI_MODEL or k3)
    #[arg(long)]
    pub model: Option<String>,
    /// Extra system-prompt instructions (appended to the base prompt; also
    /// settable via $KIMI_SYSTEM_PROMPT)
    #[arg(long)]
    pub system: Option<String>,
    /// File with extra system-prompt instructions
    #[arg(long)]
    pub system_file: Option<PathBuf>,
    /// Print the fully composed system prompt and exit (no API call)
    #[arg(long)]
    pub print_system: bool,
}

/// REPL commands, printed by /help. Keep in sync with the dispatch in run().
const COMMANDS: &[(&str, &str)] = &[
    ("/help", "list available commands"),
    ("/compact", "summarize older messages now (happens automatically near the window)"),
    ("/context", "show context size and session token usage"),
    ("/model", "show requested and API-reported model"),
    ("/reset", "clear conversation history"),
    ("/system", "print the full system prompt"),
    ("/usage", "show subscription usage limits"),
    ("/yolo", "toggle run_command auto-approval"),
    ("/quit", "quit (also: exit, Ctrl-C, Ctrl-D)"),
];

static AT_PROMPT: AtomicBool = AtomicBool::new(false); // true while blocked in readline

/// Print to stderr without a newline, dimmed when on a terminal.
fn dim_err(s: &str) {
    if std::io::stderr().is_terminal() {
        eprint!("\x1b[2m{s}\x1b[0m");
    } else {
        eprint!("{s}");
    }
    std::io::stderr().flush().ok();
}

fn exit_msg(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1);
}

struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = if std::io::stderr().is_terminal() {
            let stop2 = Arc::clone(&stop);
            Some(thread::spawn(move || {
                let glyphs = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let mut i = 0;
                while !stop2.load(Ordering::Relaxed) {
                    eprint!("\r\x1b[2m{} waiting for model\x1b[0m", glyphs[i % glyphs.len()]);
                    std::io::stderr().flush().ok();
                    i += 1;
                    thread::sleep(Duration::from_millis(100));
                }
            }))
        } else {
            None
        };
        Spinner { stop, handle }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
        }
    }
}

/// User input read while a turn is running (interjections, ask_user answers).
/// None once stdin hits EOF so `select!` branches on it can be disabled.
type StdinInput = Option<Lines<tokio::io::BufReader<tokio::io::Stdin>>>;

/// The terminal's [TurnIo]: owns stdin, the spinner, and the markdown
/// renderer, and tracks half-printed reasoning/content lines so they can be
/// closed cleanly at stream end.
struct TerminalIo {
    stdin: StdinInput,
    cancel: Arc<AtomicBool>,
    spinner: Option<Spinner>,
    renderer: StreamRenderer,
    reasoning_open: bool, // reasoning printed to stderr without trailing newline
    content_open: bool,   // content printed to stdout without trailing newline
}

impl TerminalIo {
    fn new(cancel: Arc<AtomicBool>) -> TerminalIo {
        TerminalIo {
            stdin: Some(tokio::io::BufReader::new(tokio::io::stdin()).lines()),
            cancel,
            spinner: None,
            renderer: StreamRenderer::new(),
            reasoning_open: false,
            content_open: false,
        }
    }

    /// Non-blocking-ish read of a queued input line (5ms grace for the stdin
    /// thread to deliver). Used at tool-call boundaries and to drain extra
    /// lines after an interjection. Returns trimmed non-empty lines only.
    async fn pending_line_impl(&mut self) -> Option<String> {
        let lines = self.stdin.as_mut()?;
        match tokio::time::timeout(Duration::from_millis(5), lines.next_line()).await {
            Ok(Ok(Some(l))) if !l.trim().is_empty() => Some(l.trim().to_string()),
            Ok(Ok(None)) => {
                self.stdin = None; // EOF: stop polling stdin
                None
            }
            _ => None,
        }
    }
}

impl TurnIo for TerminalIo {
    fn waiting(&mut self) {
        // A new round trip: reset the renderer and start the spinner.
        self.renderer = StreamRenderer::new();
        self.reasoning_open = false;
        self.content_open = false;
        if self.spinner.is_none() {
            self.spinner = Some(Spinner::start());
        }
    }

    fn first_token(&mut self) {
        drop(self.spinner.take());
    }

    fn reasoning(&mut self, delta: &str) {
        dim_err(delta);
        self.reasoning_open = true;
    }

    fn content(&mut self, delta: &str) {
        if self.reasoning_open {
            eprintln!();
            self.reasoning_open = false;
        }
        self.renderer.push(delta);
        self.content_open = true;
    }

    fn stream_end(&mut self) {
        drop(self.spinner.take());
        self.renderer.finish();
        if self.reasoning_open {
            eprintln!();
            self.reasoning_open = false;
        }
        if self.content_open {
            println!();
            self.content_open = false;
        }
    }

    fn note(&mut self, text: &str) {
        harness::log(text);
    }

    async fn steer(&mut self) -> Steer {
        // Ctrl-C aborts; a typed line is an interjection (any further queued
        // lines are drained into the same message). Once stdin is closed the
        // line branch never resolves, which effectively disables it.
        tokio::select! {
            _ = cancelled(&self.cancel) => Steer::Interrupted,
            line = next_input(&mut self.stdin) => match line {
                Some(first) => {
                    let mut text = first;
                    while let Some(more) = self.pending_line_impl().await {
                        text.push('\n');
                        text.push_str(&more);
                    }
                    Steer::Interjected(text)
                }
                None => std::future::pending().await,
            },
        }
    }

    async fn pending_line(&mut self) -> Option<String> {
        self.pending_line_impl().await
    }

    async fn ask(&mut self, question: &str) -> Result<String, String> {
        enum Answer {
            Line(String),
            Empty,
            Eof,
            Interrupted,
            Error(String),
        }
        eprintln!("\n\x1b[36m[question] {question}\x1b[0m");
        loop {
            eprint!("answer> ");
            std::io::stderr().flush().ok();
            let answer = match self.stdin.as_mut() {
                None => return Err("error: stdin unavailable — cannot ask the user".into()),
                Some(lines) => tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(l)) if l.trim().is_empty() => Answer::Empty,
                        Ok(Some(l)) => Answer::Line(l.trim().to_string()),
                        Ok(None) => Answer::Eof,
                        Err(e) => Answer::Error(format!("error: reading answer: {e}")),
                    },
                    _ = cancelled(&self.cancel) => Answer::Interrupted,
                },
            };
            match answer {
                Answer::Line(l) => return Ok(l),
                Answer::Empty => continue, // re-prompt
                Answer::Eof => {
                    self.stdin = None;
                    return Err("error: stdin closed — no answer available".into());
                }
                Answer::Interrupted => return Err("error: interrupted by user".into()),
                Answer::Error(e) => return Err(e),
            }
        }
    }

    async fn confirm(&mut self, command: &str) -> bool {
        eprintln!("\n\x1b[33m[confirm] run: {command}\x1b[0m");
        eprint!("allow? [y/N] ");
        std::io::stderr().flush().ok();
        // Blocking read_line, but off the async runtime threads.
        let answer = tokio::task::spawn_blocking(|| {
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).ok();
            answer
        })
        .await
        .unwrap_or_default();
        if self.cancel.load(Ordering::SeqCst) {
            return false;
        }
        matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

/// Next non-empty input line; None once stdin is closed.
async fn next_input(stdin: &mut StdinInput) -> Option<String> {
    loop {
        match stdin.as_mut()?.next_line().await {
            Ok(Some(l)) if !l.trim().is_empty() => return Some(l.trim().to_string()),
            Ok(Some(_)) => continue, // empty line
            Ok(None) | Err(_) => {
                *stdin = None;
                return None;
            }
        }
    }
}

// ------------------------------------------------------------ repl commands

/// Rough local token estimate from character count (~4 chars/token for
/// English/code). The API's own prompt_tokens (shown as "last request") is
/// the exact figure; this estimate exists to size individual messages.
/// The /context command: what's currently in the conversation and how big it
/// is. Per-message sizes are local estimates; the per-request and session
/// totals come from the API's usage reports and are exact. The last request is
/// also shown against the session's context window — the same number
/// compaction triggers on.
fn show_context(session: &Session) {
    let messages = &session.messages;
    let stats = &session.stats;
    let mut by_role: BTreeMap<&str, u64> = BTreeMap::new();
    let mut sizes: Vec<(usize, &str, u64)> = Vec::new(); // (index, role, est. tokens)
    let mut total_chars = 0usize;
    for (i, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("?");
        *by_role.entry(role).or_default() += 1;
        let chars = message_chars(m);
        total_chars += chars;
        sizes.push((i, role, est_tokens(chars)));
    }

    println!("Context");
    let roles = by_role.iter().map(|(r, n)| format!("{n} {r}")).collect::<Vec<_>>().join(", ");
    println!("  messages       {} ({roles})", messages.len());
    println!("  history size   ~{} tokens (estimated)", fmt_num(est_tokens(total_chars)));
    match stats.last {
        Some(t) => {
            print!(
                "  last request   {} prompt + {} completion tokens (measured)",
                fmt_num(t.prompt),
                fmt_num(t.completion)
            );
            let window = session.context_window;
            let pct = t.prompt as f64 / window as f64 * 100.0;
            print!(" — {pct:.0}% of the {} window", fmt_num(window));
            println!();
        }
        None => println!("  last request   no API round trip yet this session"),
    }
    println!(
        "  session total  {} prompt + {} completion tokens over {} round trips",
        fmt_num(stats.prompt_tokens),
        fmt_num(stats.completion_tokens),
        stats.round_trips
    );
    sizes.sort_by_key(|&(_, _, t)| std::cmp::Reverse(t));
    println!("  largest messages (estimated):");
    for (i, role, tokens) in sizes.iter().take(5) {
        println!("    #{i:<4} {role:<9} ~{} tokens", fmt_num(*tokens));
    }
}

/// The /model command: the model id requested by the harness, and the one the
/// API reported serving if its responses include a `model` field.
fn show_model(session: &Session) {
    println!("requested model    {}", session.model);
    match &session.stats.served_model {
        Some(served) if *served == session.model => {
            println!("API-reported model {served} (matches request)");
        }
        Some(served) => {
            println!("API-reported model {served} (differs from request)");
        }
        None => {
            println!("API-reported model not available yet");
            println!("  (no API response has included a model field this session)");
        }
    }
}

/// The /usage command: weekly quota, per-window limits (with reset hints),
/// and membership level. Errors are reported, not fatal.
async fn show_usage(session: &mut Session) {
    let payload = match usage::fetch_usage(&session.client, &mut session.auth, &session.base_url).await {
        Ok(p) => p,
        Err(e) => {
            harness::log(&format!("[usage] {e}"));
            return;
        }
    };
    let mut rows = Vec::new();
    if let Some(row) = usage::usage_row(&payload["usage"], "Weekly limit") {
        rows.push(row);
    }
    if let Some(limits) = payload["limits"].as_array() {
        for (i, item) in limits.iter().enumerate() {
            let detail = if item["detail"].is_object() { &item["detail"] } else { item };
            if let Some(row) = usage::usage_row(detail, &usage::limit_label(item, detail, i)) {
                rows.push(row);
            }
        }
    }
    let header = match payload["user"]["membership"]["level"].as_str() {
        Some(level) => format!(
            "Plan usage (membership: {})",
            level.strip_prefix("LEVEL_").unwrap_or(level).to_lowercase()
        ),
        None => "Plan usage".to_string(),
    };
    println!("{header}");
    if rows.is_empty() {
        println!("  no usage data available");
    }
    let parallel = usage::usage_int(&payload["parallel"]["limit"]);
    let width = rows
        .iter()
        .map(|r| r.label.len())
        .chain(parallel.map(|_| "Parallel sessions".len()))
        .max()
        .unwrap_or(0);
    for row in rows {
        let pct = if row.limit > 0 {
            format!(" ({:.0}%)", row.used as f64 / row.limit as f64 * 100.0)
        } else {
            String::new()
        };
        let reset = row.reset_hint.map(|h| format!("  {h}")).unwrap_or_default();
        println!("  {:<width$}  {}/{}{}{}", row.label, row.used, row.limit, pct, reset);
    }
    if let Some(parallel) = parallel {
        println!("  {:<width$}  {parallel}", "Parallel sessions");
    }
}

// ------------------------------------------------------------ entry point

pub async fn run(args: ReplArgs) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut config = SessionConfig::from_env(cwd, !args.no_yolo);
    if let Some(model) = args.model {
        config.model = model;
    }
    config.system_extra = args.system;
    config.system_file = args.system_file;

    if args.print_system {
        match harness::prompt::build_system_prompt(&config) {
            Ok(prompt) => println!("{prompt}"),
            Err(e) => exit_msg(&e),
        }
        return;
    }

    // SIGINT no longer kills the process outright:
    //   - at the prompt: quit (cooked-mode terminals deliver SIGINT instead of
    //     a readline interrupt, so handle it here, not only via ReadlineError)
    //   - mid-turn: set the session's cancel flag so blocking tool code aborts
    //     and the streaming side aborts the current turn
    //   - a second Ctrl-C force-quits
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = Arc::clone(&cancel);
        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
                if AT_PROMPT.load(Ordering::SeqCst) {
                    exit(0);
                }
                if cancel.swap(true, Ordering::SeqCst) {
                    exit(130); // already set: second Ctrl-C, force quit
                }
            }
        });
    }

    let mut session = match Session::start(config).await {
        Ok(s) => s,
        Err(e) => exit_msg(&e),
    };
    session.cancel = cancel;
    let mode = if session.yolo { "yolo" } else { "confirm" };
    harness::log(&format!(
        "harness 0.2 — model={} mode={mode} cwd={} (/help for commands; type a line mid-turn to steer; exit or Ctrl-C to quit)",
        session.model,
        session.cwd.display()
    ));

    let mut io = TerminalIo::new(Arc::clone(&session.cancel));

    if let Some(prompt) = args.prompt {
        session.push_user(&prompt);
        if let Some(fatal) = session.run_turn(&mut io).await {
            exit_msg(&fatal);
        }
        return;
    }

    let mut rl = DefaultEditor::new().unwrap_or_else(|e| exit_msg(&format!("readline init: {e}")));
    loop {
        session.cancel.store(false, Ordering::SeqCst); // clear any Ctrl-C from the last turn
        AT_PROMPT.store(true, Ordering::SeqCst);
        let input = rl.readline("\nyou> ");
        AT_PROMPT.store(false, Ordering::SeqCst);
        match input {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if matches!(line, "exit" | "quit" | "/exit" | "/quit") {
                    break;
                }
                if line == "/help" {
                    for (cmd, desc) in COMMANDS {
                        println!("  {cmd:<8} {desc}");
                    }
                    continue;
                }
                if line == "/context" {
                    show_context(&session);
                    continue;
                }
                if line == "/compact" {
                    // Same path the loop takes automatically near the window;
                    // doing it on demand is how you can see what it produced.
                    match session.compact(&mut io).await {
                        Ok(report) => harness::log(&format!("[{report}]")),
                        Err(e) => harness::log(&format!("[compaction skipped: {e}]")),
                    }
                    continue;
                }
                if line == "/model" {
                    show_model(&session);
                    continue;
                }
                if line == "/reset" {
                    session.reset();
                    harness::log("[history cleared]");
                    continue;
                }
                if line == "/system" {
                    println!("{}", session.messages[0]["content"].as_str().unwrap_or(""));
                    continue;
                }
                if line == "/usage" {
                    show_usage(&mut session).await;
                    continue;
                }
                if line == "/yolo" {
                    session.yolo = !session.yolo;
                    harness::log(&format!(
                        "[yolo mode {}]",
                        if session.yolo {
                            "on — commands auto-approved"
                        } else {
                            "off — commands need confirmation"
                        }
                    ));
                    continue;
                }
                rl.add_history_entry(line).ok();
                session.push_user(line);
                if let Some(fatal) = session.run_turn(&mut io).await {
                    harness::log(&format!("[fatal] {fatal}"));
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                break; // Ctrl-C at the prompt: quit
            }
            Err(ReadlineError::Eof) => break, // Ctrl-D: quit
            Err(e) => exit_msg(&format!("readline: {e}")),
        }
    }
}
