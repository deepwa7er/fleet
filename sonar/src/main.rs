//! sonar — replay a captured chatbot request against a list of prompts,
//! through an intercepting proxy.
//!
//! You capture one request out of the proxy, mark where the message goes, and
//! point sonar at a prompt list. It fires each prompt as a faithful copy of
//! that request — same headers, same auth, same body shape — routes every hit
//! back through the proxy so the traffic lands in its history, extracts the
//! assistant's reply, and writes a complete JSON results file.

mod client;
mod prompts;
mod report;
mod response;
mod template;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::client::{TlsTrust, build, to_request};
use crate::report::{Outcome, Report};
use crate::response::extract;
use crate::template::{Escape, Template};

/// Replay a captured chatbot request against a list of prompts, through a proxy.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Raw HTTP request saved from the proxy, with a marker where the prompt goes.
    #[arg(short, long)]
    request: PathBuf,

    /// Prompts to fire: a `.json` array of strings, or a wordlist (one per line).
    #[arg(short, long)]
    prompts: PathBuf,

    /// Marker in the request template that each prompt replaces.
    #[arg(long, default_value = "§PROMPT§")]
    marker: String,

    /// How to encode a prompt before it replaces the marker.
    #[arg(long, default_value = "json")]
    escape: Escape,

    /// URL scheme for the target; a saved request does not record it.
    #[arg(long, default_value = "https")]
    scheme: String,

    /// Intercepting proxy to route through, e.g. http://127.0.0.1:8080 for Burp.
    #[arg(long)]
    proxy: Option<String>,

    /// Proxy CA certificate (PEM or DER) to trust, keeping full verification.
    #[arg(long)]
    proxy_ca: Option<PathBuf>,

    /// Skip TLS verification instead of trusting a proxy CA. Off by default.
    #[arg(long)]
    insecure: bool,

    /// JSON pointer to the reply in a JSON response, overriding the heuristics.
    #[arg(long)]
    reply_pointer: Option<String>,

    /// JSON pointer to the per-chunk delta in an SSE response, overriding heuristics.
    #[arg(long)]
    delta_pointer: Option<String>,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 120)]
    timeout: u64,

    /// Requests in flight at once. One keeps the proxy history strictly ordered.
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// Delay in milliseconds between dispatching successive requests.
    #[arg(long, default_value_t = 0)]
    delay: u64,

    /// Where to write the JSON results file.
    #[arg(short, long, default_value = "sonar-results.json")]
    out: PathBuf,

    /// Print the results as JSON on stdout instead of a table.
    #[arg(long)]
    json: bool,
}

/// The shared inputs each per-prompt task borrows.
struct Run {
    client: reqwest::Client,
    template: Template,
    escape: Escape,
    reply_pointer: Option<String>,
    delta_pointer: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.concurrency == 0 {
        anyhow::bail!("--concurrency must be at least 1");
    }
    if cli.proxy.is_none() && (cli.proxy_ca.is_some() || cli.insecure) {
        eprintln!(
            "warning: TLS-trust flags have no effect without --proxy; \
             requests will go straight to the target."
        );
    }

    let raw = std::fs::read_to_string(&cli.request)
        .with_context(|| format!("reading request template from {}", cli.request.display()))?;
    let template = Template::new(raw, cli.marker.clone(), cli.scheme.clone())?;
    let prompts = prompts::load(&cli.prompts)?;

    let trust = TlsTrust::resolve(cli.proxy_ca.as_deref(), cli.insecure)?;
    let http = build(cli.proxy.as_deref(), &trust, Duration::from_secs(cli.timeout))?;

    // Resolve the target URL once for the report header (prompt 0, unsent).
    let target = template
        .fill(prompts.first().map(String::as_str).unwrap_or(""), cli.escape)
        .map(|f| f.url)
        .unwrap_or_else(|_| "<unresolved>".to_string());

    let run = Arc::new(Run {
        client: http,
        template,
        escape: cli.escape,
        reply_pointer: cli.reply_pointer.clone(),
        delta_pointer: cli.delta_pointer.clone(),
    });

    let started_at = chrono::Local::now().to_rfc3339();
    if !cli.json {
        eprintln!(
            "sonar: {} prompt(s) → {}{}",
            prompts.len(),
            target,
            cli.proxy
                .as_deref()
                .map(|p| format!("  via {p}"))
                .unwrap_or_default(),
        );
    }

    let outcomes = dispatch(run, prompts, cli.concurrency, cli.delay, cli.json).await;

    let report = Report {
        target,
        started_at,
        prompt_count: outcomes.len(),
        outcomes,
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let json = serde_json::to_string_pretty(&report)?;
        tokio::fs::write(&cli.out, json)
            .await
            .with_context(|| format!("writing results to {}", cli.out.display()))?;
        eprintln!("\nsonar: wrote full results to {}", cli.out.display());
    }

    Ok(())
}

/// Fire every prompt, honouring the concurrency cap and inter-dispatch delay,
/// and return the outcomes in prompt order.
async fn dispatch(
    run: Arc<Run>,
    prompts: Vec<String>,
    concurrency: usize,
    delay: u64,
    quiet: bool,
) -> Vec<Outcome> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks: JoinSet<Outcome> = JoinSet::new();

    for (index, prompt) in prompts.into_iter().enumerate() {
        // Acquire before spawning so the delay paces real dispatch, not queueing.
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore is never closed");
        let run = run.clone();
        tasks.spawn(async move {
            let outcome = fire_one(&run, index, prompt).await;
            drop(permit);
            if !quiet {
                println!("{}\n", outcome.render_console());
            }
            outcome
        });

        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    let mut outcomes = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(outcome) => outcomes.push(outcome),
            Err(join_err) => eprintln!("warning: a request task panicked: {join_err}"),
        }
    }
    outcomes.sort_by_key(|o| o.index);
    outcomes
}

/// Send one prompt and record what came back. Transport failures are captured
/// as an outcome rather than aborting the run — one dead request should not
/// sink the rest of the list.
async fn fire_one(run: &Run, index: usize, prompt: String) -> Outcome {
    let filled = match run.template.fill(&prompt, run.escape) {
        Ok(filled) => filled,
        Err(err) => return failed(index, prompt, 0, format!("building request: {err:#}")),
    };
    let request = match to_request(&run.client, &filled) {
        Ok(request) => request,
        Err(err) => return failed(index, prompt, 0, format!("building request: {err:#}")),
    };

    let start = Instant::now();
    let response = match run.client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            let ms = start.elapsed().as_millis();
            return failed(index, prompt, ms, format!("{err:#}"));
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = match response.text().await {
        Ok(body) => body,
        Err(err) => {
            let ms = start.elapsed().as_millis();
            let mut o = failed(index, prompt, ms, format!("reading body: {err:#}"));
            o.status = Some(status);
            o.content_type = content_type;
            return o;
        }
    };
    let latency_ms = start.elapsed().as_millis();

    let pointer = if content_type
        .as_deref()
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/event-stream"))
    {
        run.delta_pointer.as_deref()
    } else {
        run.reply_pointer.as_deref()
    };
    let reply = extract(content_type.as_deref(), &body, pointer);

    Outcome {
        index,
        prompt,
        status: Some(status),
        latency_ms,
        content_type,
        streamed: reply.streamed,
        reply: Some(reply.text),
        body: Some(body),
        error: None,
    }
}

fn failed(index: usize, prompt: String, latency_ms: u128, error: String) -> Outcome {
    Outcome {
        index,
        prompt,
        status: None,
        latency_ms,
        content_type: None,
        streamed: false,
        reply: None,
        body: None,
        error: Some(error),
    }
}
