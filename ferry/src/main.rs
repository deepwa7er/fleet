mod config;
mod resolve;
mod suggest;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::header::{CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::config::{Config, validate_command_name, validate_command_url};
use crate::resolve::Resolution;

/// The repo's example config doubles as the baseline written on first run,
/// so the two can never drift apart.
const BASELINE_CONFIG: &str = include_str!("../ferry.toml");

struct AppState {
    config_path: PathBuf,
    /// Serializes config-file mutations so concurrent writes can't interleave
    /// into a lost update. Reads stay lock-free (the file is re-read per request).
    write_lock: Mutex<()>,
}

/// Failure from a config-mutating operation, split so the HTTP layer can map a
/// user mistake (e.g. a name collision) to 400 and a genuine IO/parse failure
/// to 500.
enum WriteError {
    Rejected(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for WriteError {
    fn from(err: anyhow::Error) -> Self {
        WriteError::Internal(err)
    }
}

impl AppState {
    /// Apply `edit` to the parsed `[commands]` table (created if absent) under
    /// the write lock, then persist atomically — so existing comments and
    /// formatting survive and a reader never sees a partially written file.
    /// The closure may reject the change with a user-facing message.
    fn mutate_commands<F>(&self, edit: F) -> Result<(), WriteError>
    where
        F: FnOnce(&mut toml_edit::Table) -> Result<(), WriteError>,
    {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let text = std::fs::read_to_string(&self.config_path)
            .with_context(|| format!("failed to read {}", self.config_path.display()))?;
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("failed to parse {}", self.config_path.display()))?;

        let commands = doc
            .entry("commands")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .context("`commands` exists but is not a table")?;

        edit(commands)?;

        write_atomic(&self.config_path, doc.to_string().as_bytes())?;
        Ok(())
    }

    /// Add or overwrite every name, all pointing at the same URL. Several names
    /// for one URL are just several entries — that is how aliases work.
    fn add_commands(&self, names: &[String], url: &str) -> Result<(), WriteError> {
        self.mutate_commands(|commands| {
            for name in names {
                commands.insert(name, toml_edit::value(url));
            }
            Ok(())
        })
    }

    /// Update a command, optionally renaming it. Renaming onto a *different*
    /// existing command is rejected so an edit can't silently clobber another.
    fn edit_command(&self, original: &str, name: &str, url: &str) -> Result<(), WriteError> {
        self.mutate_commands(|commands| {
            if !commands.contains_key(original) {
                return Err(WriteError::Rejected(format!(
                    "No command named {original:?} to edit — it may have just been removed."
                )));
            }
            if name != original && commands.contains_key(name) {
                return Err(WriteError::Rejected(format!(
                    "A command named {name:?} already exists."
                )));
            }
            if name != original {
                commands.remove(original);
            }
            commands.insert(name, toml_edit::value(url));
            Ok(())
        })
    }

    /// Remove a command. A missing name is reported rather than silently
    /// succeeding, so the UI can explain why nothing changed.
    fn delete_command(&self, name: &str) -> Result<(), WriteError> {
        self.mutate_commands(|commands| {
            if commands.remove(name).is_none() {
                return Err(WriteError::Rejected(format!("No command named {name:?}.")));
            }
            Ok(())
        })
    }
}

/// Replace a file's contents atomically: write a sibling temp file, then rename
/// over the target. Rename is atomic on the same filesystem, so a crash leaves
/// either the old file or the new one intact, never a truncated mix.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .context("config path has no file name")?
        .to_string_lossy();
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, bytes)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path = match explicit_config_path()? {
        Some(path) => path,
        None => {
            let path = default_config_path()?;
            if !path.exists() {
                write_baseline_config(&path)?;
            }
            path
        }
    };
    // Load once up front so a broken config fails at startup, and to learn the port.
    let config = Config::load(&config_path)?;

    let app = Router::new()
        .route("/", get(handle_query))
        .route("/commands", get(handle_commands).post(handle_add_command))
        .route("/commands/edit", post(handle_edit_command))
        .route("/commands/delete", post(handle_delete_command))
        .route("/suggest", get(handle_suggest))
        .route("/opensearch.xml", get(handle_opensearch))
        .with_state(Arc::new(AppState {
            config_path,
            write_lock: Mutex::new(()),
        }));

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    println!("ferry listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// The config path given on the command line, if any. An explicit path that
/// doesn't exist is an error (likely a typo), so no baseline is written for it.
fn explicit_config_path() -> anyhow::Result<Option<PathBuf>> {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => Ok(None),
        (Some(path), None) => Ok(Some(PathBuf::from(path))),
        (Some(_), Some(_)) => anyhow::bail!("usage: ferry [config-path]"),
    }
}

fn default_config_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; pass a config path")?;
    Ok(PathBuf::from(home).join(".config/ferry/ferry.toml"))
}

fn write_baseline_config(path: &Path) -> anyhow::Result<()> {
    let dir = path.parent().context("config path has no parent directory")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(path, BASELINE_CONFIG)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("created starter config at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_config_is_valid() {
        Config::from_toml(BASELINE_CONFIG).expect("bundled ferry.toml must parse and validate");
    }

    #[test]
    fn origin_falls_back_to_http_and_host() {
        assert_eq!(build_origin(None, None, Some("localhost:7777")), "http://localhost:7777");
    }

    #[test]
    fn origin_defaults_host_when_absent() {
        assert_eq!(build_origin(None, None, None), "http://localhost");
    }

    #[test]
    fn origin_uses_forwarded_proto_and_host() {
        // What `tailscale serve` presents: TLS terminated, Host preserved.
        assert_eq!(
            build_origin(Some("https"), None, Some("vps.example.ts.net")),
            "https://vps.example.ts.net",
        );
    }

    #[test]
    fn origin_prefers_forwarded_host_over_host() {
        assert_eq!(
            build_origin(Some("https"), Some("ferry.example.ts.net"), Some("127.0.0.1:7777")),
            "https://ferry.example.ts.net",
        );
    }

    #[test]
    fn origin_takes_first_entry_of_forwarded_chain() {
        assert_eq!(
            build_origin(Some("https, http"), Some("outer.example, inner.example"), None),
            "https://outer.example",
        );
    }

    #[test]
    fn origin_rejects_bogus_scheme() {
        assert_eq!(build_origin(Some("javascript"), None, Some("host")), "http://host");
    }
}

#[derive(Deserialize)]
struct QueryParams {
    #[serde(default)]
    q: String,
}

async fn handle_query(
    State(state): State<Arc<AppState>>,
    Query(params): Query<QueryParams>,
) -> Response {
    let config = match load_config(&state) {
        Ok(config) => config,
        Err(response) => return response,
    };
    match resolve::resolve(&config, &params.q) {
        Resolution::Redirect(url) => Redirect::temporary(&url).into_response(),
        Resolution::ListPage => Redirect::temporary("/commands").into_response(),
    }
}

/// OpenSearch suggestions: `["<input>", [completions], [descriptions]]`.
/// Browsers query this as you type and offer the completions in the dropdown.
async fn handle_suggest(
    State(state): State<Arc<AppState>>,
    Query(params): Query<QueryParams>,
) -> Response {
    let config = match load_config(&state) {
        Ok(config) => config,
        Err(response) => return response,
    };
    let suggestions = suggest::suggest(&config, &params.q);
    let completions: Vec<&str> = suggestions.iter().map(|s| s.completion.as_str()).collect();
    let descriptions: Vec<&str> = suggestions.iter().map(|s| s.description.as_str()).collect();
    let body = serde_json::json!([params.q, completions, descriptions]).to_string();
    ([(CONTENT_TYPE, "application/x-suggestions+json")], body).into_response()
}

/// OpenSearch descriptor advertising the search and suggestion endpoints.
/// The URLs are built from the origin the client actually used, so the
/// descriptor stays correct whether ferry is reached directly on loopback or
/// through a reverse proxy such as `tailscale serve` (which terminates TLS and
/// forwards to localhost over plain HTTP).
async fn handle_opensearch(headers: HeaderMap) -> Response {
    let origin = escape_html(&external_origin(&headers));
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
  <ShortName>ferry</ShortName>
  <Description>ferry address-bar shortcuts</Description>
  <InputEncoding>UTF-8</InputEncoding>
  <Url type="text/html" method="get" template="{origin}/?q={{searchTerms}}"/>
  <Url type="application/x-suggestions+json" method="get" template="{origin}/suggest?q={{searchTerms}}"/>
</OpenSearchDescription>
"#
    );
    ([(CONTENT_TYPE, "application/opensearchdescription+xml")], body).into_response()
}

/// The `scheme://authority` the client used to reach ferry, as seen through any
/// reverse proxy in front of it. Loopback access has no proxy headers and falls
/// back to `http` and the direct `Host`. Trusting these headers is safe because
/// ferry binds to loopback only — the sole remote client is the local proxy.
fn external_origin(headers: &HeaderMap) -> String {
    let forwarded_proto = header_str(headers, "x-forwarded-proto");
    let forwarded_host = header_str(headers, "x-forwarded-host");
    let host = header_str(headers, HOST.as_str());
    build_origin(forwarded_proto, forwarded_host, host)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn build_origin(
    forwarded_proto: Option<&str>,
    forwarded_host: Option<&str>,
    host: Option<&str>,
) -> String {
    // Forwarded headers can carry a comma-separated chain; the first entry is
    // the value the original client sent.
    let scheme = forwarded_proto
        .and_then(first_token)
        .map(str::to_ascii_lowercase)
        .filter(|scheme| scheme == "http" || scheme == "https")
        .unwrap_or_else(|| "http".to_string());
    let host = forwarded_host
        .and_then(first_token)
        .or(host)
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

fn first_token(value: &str) -> Option<&str> {
    let token = value.split(',').next()?.trim();
    (!token.is_empty()).then_some(token)
}

#[derive(Deserialize, Default)]
struct CommandsQuery {
    /// Names involved in the just-completed action, set by the
    /// post/redirect/get round-trip so the listing can confirm what changed.
    added: Option<String>,
    updated: Option<String>,
    removed: Option<String>,
}

async fn handle_commands(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CommandsQuery>,
) -> Response {
    let config = match load_config(&state) {
        Ok(config) => config,
        Err(response) => return response,
    };
    let notice = success_notice(&query);
    Html(render_commands_page(&config, notice.as_ref(), &FormValues::default())).into_response()
}

/// Build the confirmation banner for whichever action just redirected here.
fn success_notice(query: &CommandsQuery) -> Option<Notice> {
    let (verb, names) = if let Some(names) = &query.added {
        ("Added", names)
    } else if let Some(names) = &query.updated {
        ("Updated", names)
    } else if let Some(names) = &query.removed {
        ("Removed", names)
    } else {
        return None;
    };
    Some(Notice { error: false, message: format!("{verb} \u{201c}{names}\u{201d}.") })
}

#[derive(Deserialize)]
struct NewCommand {
    /// One or more whitespace-separated names, all mapped to the same URL.
    names: String,
    url: String,
}

/// Add one or more aliases from the `/commands` form, all pointing at one URL.
async fn handle_add_command(
    State(state): State<Arc<AppState>>,
    Form(form): Form<NewCommand>,
) -> Response {
    let raw_names = form.names.trim();
    let url = form.url.trim();
    let names: Vec<String> = raw_names.split_whitespace().map(str::to_string).collect();
    let entered = || FormValues { names: raw_names.to_string(), url: url.to_string() };

    let validation = (|| {
        if names.is_empty() {
            return Err("At least one command name is required.".to_string());
        }
        for name in &names {
            validate_command_name(name)?;
        }
        validate_command_url(url)
    })();
    if let Err(message) = validation {
        return rerender_error(&state, message, entered());
    }

    match state.add_commands(&names, url) {
        Ok(()) => redirect_with("added", &names.join(" ")),
        Err(WriteError::Rejected(message)) => rerender_error(&state, message, entered()),
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

#[derive(Deserialize)]
struct EditCommand {
    /// The command's current name, used to locate the row being edited.
    original: String,
    name: String,
    url: String,
}

/// Save an edit to one row, renaming the command if `name` changed.
async fn handle_edit_command(
    State(state): State<Arc<AppState>>,
    Form(form): Form<EditCommand>,
) -> Response {
    let original = form.original.trim();
    let name = form.name.trim();
    let url = form.url.trim();

    if let Err(message) = validate_command_name(name).and_then(|()| validate_command_url(url)) {
        return rerender_error(&state, message, FormValues::default());
    }
    match state.edit_command(original, name, url) {
        Ok(()) => redirect_with("updated", name),
        Err(WriteError::Rejected(message)) => rerender_error(&state, message, FormValues::default()),
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

#[derive(Deserialize)]
struct DeleteCommand {
    /// The row's name. Named `original` so the per-row form can carry it as a
    /// hidden field alongside the editable `name`.
    original: String,
}

/// Delete one row.
async fn handle_delete_command(
    State(state): State<Arc<AppState>>,
    Form(form): Form<DeleteCommand>,
) -> Response {
    let name = form.original.trim();
    match state.delete_command(name) {
        Ok(()) => redirect_with("removed", name),
        Err(WriteError::Rejected(message)) => rerender_error(&state, message, FormValues::default()),
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

/// Post/redirect/get back to the listing with a confirmation for `action`.
fn redirect_with(action: &str, names: &str) -> Response {
    let names = utf8_percent_encode(names, NON_ALPHANUMERIC);
    Redirect::to(&format!("/commands?{action}={names}")).into_response()
}

/// Re-render the listing (HTTP 400) with an error banner, preserving any
/// add-form input the user had typed.
fn rerender_error(state: &AppState, message: String, values: FormValues) -> Response {
    match load_config(state) {
        Ok(config) => {
            let notice = Notice { error: true, message };
            (
                StatusCode::BAD_REQUEST,
                Html(render_commands_page(&config, Some(&notice), &values)),
            )
                .into_response()
        }
        Err(response) => response,
    }
}

fn internal_error(err: anyhow::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("ferry failed to save: {err:#}")).into_response()
}

/// The config is re-read on every request so edits take effect immediately.
/// At address-bar request rates that costs nothing and keeps the server stateless.
fn load_config(state: &AppState) -> Result<Config, Response> {
    Config::load(&state.config_path).map_err(|err| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("ferry config error: {err:#}")).into_response()
    })
}

/// A one-off banner shown above the command form (a success confirmation or a
/// validation error).
struct Notice {
    error: bool,
    message: String,
}

/// Values to pre-fill the add form with — empty on a normal view, the rejected
/// input when re-rendering after a validation error.
#[derive(Default)]
struct FormValues {
    names: String,
    url: String,
}

fn render_commands_page(config: &Config, notice: Option<&Notice>, form: &FormValues) -> String {
    // Each row is its own form: Save posts to /commands/edit, while Delete
    // reuses the same form via `formaction` (and `formnovalidate`, so the
    // required fields don't block a delete). The hidden `original` lets an edit
    // rename the command and lets delete target the right row.
    let mut rows = String::new();
    for (name, url) in &config.commands {
        let name = escape_html(name);
        let url = escape_html(url);
        rows.push_str(&format!(
            r#"<form class="row" method="post" action="/commands/edit">
  <input type="hidden" name="original" value="{name}">
  <input name="name" value="{name}" aria-label="command name" required>
  <input name="url" value="{url}" aria-label="URL" required>
  <button type="submit">Save</button>
  <button type="submit" class="danger" formaction="/commands/delete" formnovalidate
    onclick="return confirm('Delete this command?')">Delete</button>
</form>
"#
        ));
    }

    let notice_html = match notice {
        Some(notice) => {
            let class = if notice.error { "notice error" } else { "notice ok" };
            format!("<p class=\"{class}\">{}</p>\n", escape_html(&notice.message))
        }
        None => String::new(),
    };

    let form_html = format!(
        r#"<form class="add" method="post" action="/commands">
  <input name="names" placeholder="name(s), space-separated (e.g. mail m)" value="{names}" required autofocus>
  <input name="url" placeholder="https://…  (use {{query}} for arguments)" value="{url}" required>
  <button type="submit">Add</button>
</form>
"#,
        names = escape_html(&form.names),
        url = escape_html(&form.url),
    );

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ferry commands</title>
<link rel="search" type="application/opensearchdescription+xml" href="/opensearch.xml" title="ferry">
<style>
  body {{ font-family: ui-monospace, monospace; max-width: 52rem; margin: 2rem auto; padding: 0 1rem; }}
  .notice {{ padding: 0.5rem 0.75rem; border-radius: 4px; margin: 0.5rem 0; }}
  .notice.ok {{ background: #e7f6e7; }}
  .notice.error {{ background: #fce8e6; }}
  form.add, form.row {{ display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }}
  form.add {{ margin: 1rem 0; }}
  form.row {{ padding: 0.3rem 0; border-bottom: 1px solid #ddd; }}
  input {{ padding: 0.35rem; font: inherit; }}
  form.add input[name="names"], form.row input[name="name"] {{ flex: 0 0 9rem; }}
  input[name="url"] {{ flex: 1 1 18rem; }}
  button {{ padding: 0.35rem 0.8rem; font: inherit; cursor: pointer; }}
  button.danger {{ color: #b00; }}
  .fallback {{ margin-top: 1.5rem; color: #666; }}
</style>
</head>
<body>
<h1>ferry</h1>
{notice_html}{form_html}{rows}<p class="fallback">Fallback: <code>{fallback}</code></p>
</body>
</html>
"#,
        fallback = escape_html(&config.fallback),
    )
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
