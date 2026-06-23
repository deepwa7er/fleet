mod config;
mod resolve;
mod suggest;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::extract::{Form, Query, Request, State};
use axum::http::header::{CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
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
    /// Shared client for the note-capture POST. Cheap to clone (it's an `Arc`
    /// internally) and pools connections to the loopback notes app.
    http: reqwest::Client,
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
    /// The closure may reject the change with a user-facing message, and
    /// returns a value (e.g. the names it removed) carried back to the caller.
    fn mutate_commands<F, T>(&self, edit: F) -> Result<T, WriteError>
    where
        F: FnOnce(&mut toml_edit::Table) -> Result<T, WriteError>,
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

        let value = edit(commands)?;

        write_atomic(&self.config_path, doc.to_string().as_bytes())?;
        Ok(value)
    }

    /// Add or overwrite every name, all pointing at the same URL. Several names
    /// for one URL are just several entries — that is how aliases work.
    fn add_commands(&self, names: &[String], url: &str) -> Result<(), WriteError> {
        self.mutate_commands(|commands| {
            apply_add(commands, names, url);
            Ok(())
        })
    }

    /// Replace the alias set for the entry currently at `original_url` with
    /// `names`, all pointing at `url`. See [`apply_edit`] for the exact rules.
    fn edit_group(&self, original_url: &str, names: &[String], url: &str) -> Result<(), WriteError> {
        self.mutate_commands(|commands| {
            apply_edit(commands, original_url, names, url).map_err(WriteError::Rejected)
        })
    }

    /// Remove the whole entry at `original_url`, returning the names removed so
    /// the UI can confirm exactly what changed. A missing entry is reported
    /// rather than silently succeeding.
    fn delete_group(&self, original_url: &str) -> Result<Vec<String>, WriteError> {
        self.mutate_commands(|commands| {
            apply_delete(commands, original_url).map_err(WriteError::Rejected)
        })
    }
}

/// The command names that currently point at `url`, in document order.
fn names_for_url(commands: &toml_edit::Table, url: &str) -> Vec<String> {
    commands
        .iter()
        .filter(|(_, item)| item.as_str() == Some(url))
        .map(|(key, _)| key.to_string())
        .collect()
}

/// Insert every name pointing at `url`, overwriting any existing entry of the
/// same name (the documented add-form behaviour).
fn apply_add(commands: &mut toml_edit::Table, names: &[String], url: &str) {
    for name in names {
        commands.insert(name, toml_edit::value(url));
    }
}

/// Replace the alias set of the entry currently at `original_url` with `names`,
/// all targeting `url`. Names already in the group keep their TOML line (and any
/// comment); names that leave the group are removed; new names are inserted.
/// Rejected — with a user-facing message — when the entry no longer exists, or
/// when a new name is already used by a *different* entry (so an edit can't
/// silently clobber another shortcut).
fn apply_edit(
    commands: &mut toml_edit::Table,
    original_url: &str,
    names: &[String],
    url: &str,
) -> Result<(), String> {
    let old_names = names_for_url(commands, original_url);
    if old_names.is_empty() {
        return Err(format!(
            "No entry for {original_url:?} to edit — it may have just been changed."
        ));
    }
    for name in names {
        let in_group = old_names.iter().any(|old| old == name);
        if !in_group && commands.contains_key(name) {
            return Err(format!("A command named {name:?} already exists."));
        }
    }
    for old in &old_names {
        if !names.iter().any(|name| name == old) {
            commands.remove(old);
        }
    }
    // Re-inserting a name that's already present updates its value in place and
    // preserves its comment; only a dropped name (above) loses its line.
    for name in names {
        commands.insert(name, toml_edit::value(url));
    }
    Ok(())
}

/// Remove every name in the entry at `original_url`, returning the removed
/// names. Rejected when there is no such entry.
fn apply_delete(commands: &mut toml_edit::Table, original_url: &str) -> Result<Vec<String>, String> {
    let names = names_for_url(commands, original_url);
    if names.is_empty() {
        return Err(format!("No entry for {original_url:?}."));
    }
    for name in &names {
        commands.remove(name);
    }
    Ok(names)
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

/// Log every request as `METHOD uri -> status` to stdout (captured by the
/// journal when run under systemd). Cheap, and handy for confirming requests
/// actually arrive — e.g. from a phone's browser.
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let response = next.run(request).await;
    println!("{method} {uri} -> {}", response.status().as_u16());
    response
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
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("failed to build the HTTP client")?,
        }))
        .layer(middleware::from_fn(log_request));

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

    #[test]
    fn parse_names_accepts_commas_and_whitespace() {
        let want = vec!["mail".to_string(), "m".to_string()];
        assert_eq!(parse_names("mail, m"), want);
        assert_eq!(parse_names("mail m"), want);
        assert_eq!(parse_names("mail,m"), want);
        // Stray separators and surrounding noise collapse away.
        assert_eq!(parse_names("  mail ,, , m  "), want);
        assert!(parse_names("   ").is_empty());
        assert!(parse_names(",,").is_empty());
    }

    /// Apply a table edit to a config and return the resulting `[commands]` map,
    /// re-parsed through `Config` so the output is proven still valid.
    fn commands_after(
        toml: &str,
        edit: impl FnOnce(&mut toml_edit::Table),
    ) -> std::collections::BTreeMap<String, String> {
        let mut doc = toml.parse::<toml_edit::DocumentMut>().unwrap();
        let table = doc
            .entry("commands")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .unwrap();
        edit(table);
        Config::from_toml(&doc.to_string()).unwrap().commands
    }

    const BASE: &str = r#"
fallback = "https://search.example/?q={query}"
[commands]
mail = "https://mail.example/"
m = "https://mail.example/"
cal = "https://cal.example/"
"#;

    #[test]
    fn add_inserts_every_alias_at_one_url() {
        let names = ["docs".to_string(), "d".to_string()];
        let commands = commands_after(BASE, |t| apply_add(t, &names, "https://docs.example/"));
        assert_eq!(commands["docs"], "https://docs.example/");
        assert_eq!(commands["d"], "https://docs.example/");
    }

    #[test]
    fn edit_adds_an_alias_to_an_existing_entry() {
        // The mail entry currently has {mail, m}; add `gmail` to it.
        let names = ["mail".to_string(), "m".to_string(), "gmail".to_string()];
        let commands = commands_after(BASE, |t| {
            apply_edit(t, "https://mail.example/", &names, "https://mail.example/").unwrap()
        });
        assert_eq!(commands["gmail"], "https://mail.example/");
        assert_eq!(commands["mail"], "https://mail.example/");
        assert_eq!(commands["m"], "https://mail.example/");
        // The unrelated entry is untouched.
        assert_eq!(commands["cal"], "https://cal.example/");
    }

    #[test]
    fn edit_can_drop_an_alias_and_change_the_url_together() {
        // Replace {mail, m} with just {mail}, pointed at a new URL.
        let names = ["mail".to_string()];
        let commands = commands_after(BASE, |t| {
            apply_edit(t, "https://mail.example/", &names, "https://newmail.example/").unwrap()
        });
        assert_eq!(commands["mail"], "https://newmail.example/");
        assert!(!commands.contains_key("m"), "the dropped alias is gone");
    }

    #[test]
    fn edit_rejects_stealing_another_entrys_name() {
        let names = ["mail".to_string(), "cal".to_string()];
        let err = commands_after(BASE, |t| {
            let result = apply_edit(t, "https://mail.example/", &names, "https://mail.example/");
            assert_eq!(result, Err("A command named \"cal\" already exists.".to_string()));
        });
        // Nothing changed: `cal` still points where it did.
        assert_eq!(err["cal"], "https://cal.example/");
    }

    #[test]
    fn edit_rejects_a_vanished_entry() {
        commands_after(BASE, |t| {
            let result = apply_edit(t, "https://gone.example/", &["x".to_string()], "https://x.example/");
            assert!(matches!(result, Err(message) if message.contains("No entry for")));
        });
    }

    #[test]
    fn delete_removes_every_alias_in_the_entry() {
        let mut removed = Vec::new();
        let commands = commands_after(BASE, |t| {
            removed = apply_delete(t, "https://mail.example/").unwrap();
        });
        removed.sort();
        assert_eq!(removed, vec!["m".to_string(), "mail".to_string()]);
        assert!(!commands.contains_key("mail"));
        assert!(!commands.contains_key("m"));
        assert_eq!(commands["cal"], "https://cal.example/");
    }

    #[test]
    fn delete_rejects_a_vanished_entry() {
        commands_after(BASE, |t| {
            assert_eq!(
                apply_delete(t, "https://gone.example/"),
                Err("No entry for \"https://gone.example/\".".to_string()),
            );
        });
    }

    /// A row not being edited is read-only: plain text plus an Edit link, no
    /// name/URL inputs and no Save/Delete controls for that entry.
    #[test]
    fn listing_rows_are_read_only_by_default() {
        let config = Config::from_toml(BASE).unwrap();
        let page = render_commands_page(&config, None, &FormValues::default(), None);
        // The mail entry shows its aliases as text and offers an Edit link.
        assert!(page.contains(r#"<span class="cell-names">m, mail</span>"#));
        assert!(page.contains("/commands?edit=https%3A%2F%2Fmail%2Eexample%2F#edit"));
        // No row is in edit mode, so no Save/Delete controls are present.
        assert!(!page.contains("class=\"row editing\""));
        assert!(!page.contains("formaction=\"/commands/delete\""));
    }

    /// Opening one entry for editing turns only that row into a form (inputs +
    /// Save/Delete/Cancel); the others stay read-only.
    #[test]
    fn editing_one_row_shows_its_form_only() {
        let config = Config::from_toml(BASE).unwrap();
        let editing = EditTarget { url: "https://mail.example/".to_string(), values: None };
        let page = render_commands_page(&config, None, &FormValues::default(), Some(&editing));
        // The edited row is now a form prefilled from the entry's stored values.
        assert!(page.contains("class=\"row editing\""));
        assert!(page.contains(r#"<input type="hidden" name="original_url" value="https://mail.example/">"#));
        assert!(page.contains(r#"<input name="names" value="m, mail""#));
        assert!(page.contains("formaction=\"/commands/delete\""));
        assert!(page.contains(r#"<a class="btn" href="/commands">Cancel</a>"#));
        // The unrelated `cal` row stays read-only.
        assert!(page.contains(r#"<span class="cell-names">cal</span>"#));
    }

    /// Re-rendering after a rejected save refills the edit form with what the
    /// user typed, not the entry's stored values.
    #[test]
    fn editing_with_rejected_input_preserves_what_the_user_typed() {
        let config = Config::from_toml(BASE).unwrap();
        let editing = EditTarget {
            url: "https://mail.example/".to_string(),
            values: Some(FormValues {
                names: "mail, m, gmail".to_string(),
                url: "https://newmail.example/".to_string(),
            }),
        };
        let page = render_commands_page(&config, None, &FormValues::default(), Some(&editing));
        // Inputs carry the typed (rejected) values...
        assert!(page.contains(r#"<input name="names" value="mail, m, gmail""#));
        assert!(page.contains(r#"value="https://newmail.example/""#));
        // ...while the hidden identity stays the entry's stored URL.
        assert!(page.contains(r#"<input type="hidden" name="original_url" value="https://mail.example/">"#));
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
        Resolution::Capture { api, text, open } => {
            capture_note(&state.http, &api, &text, &open).await
        }
        Resolution::ListPage => Redirect::temporary("/commands").into_response(),
    }
}

/// POST the note text to the notes app's capture API, then return a confirmation
/// page (or, on failure, an error page that preserves the text so it isn't lost).
async fn capture_note(http: &reqwest::Client, api: &str, text: &str, open: &str) -> Response {
    let result = http
        .post(api)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            Html(render_capture_page(None, text, open)).into_response()
        }
        Ok(resp) => {
            let why = format!("the notes app returned {}", resp.status());
            (StatusCode::BAD_GATEWAY, Html(render_capture_page(Some(&why), text, open)))
                .into_response()
        }
        Err(err) => {
            let why = format!("couldn't reach the notes app: {err}");
            (StatusCode::BAD_GATEWAY, Html(render_capture_page(Some(&why), text, open)))
                .into_response()
        }
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
    /// The URL of the entry to open in edit mode, set by a row's Edit link.
    /// Absent on the default (read-only) listing.
    edit: Option<String>,
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
    // An Edit link carries the entry's URL; open that one row for editing,
    // prefilled from its stored values (`values: None`).
    let editing = query.edit.map(|url| EditTarget { url, values: None });
    Html(render_commands_page(
        &config,
        notice.as_ref(),
        &FormValues::default(),
        editing.as_ref(),
    ))
    .into_response()
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

/// Split a user- or client-entered name list into individual command names,
/// accepting commas and/or whitespace as separators and dropping empties. The
/// web form uses a comma-separated list; a programmatic client can use either,
/// so `"mail, m"`, `"mail m"`, and `"mail,m"` all yield `["mail", "m"]`.
fn parse_names(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Validate a parsed name list and its URL together, so the add and edit forms
/// reject the same bad input with the same messages.
fn validate_names_and_url(names: &[String], url: &str) -> Result<(), String> {
    if names.is_empty() {
        return Err("At least one command name is required.".to_string());
    }
    for name in names {
        validate_command_name(name)?;
    }
    validate_command_url(url)
}

#[derive(Deserialize)]
struct NewCommand {
    /// One or more names (comma- and/or whitespace-separated), all mapped to
    /// the same URL.
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
    let names = parse_names(raw_names);
    let entered = || FormValues { names: raw_names.to_string(), url: url.to_string() };

    if let Err(message) = validate_names_and_url(&names, url) {
        return rerender_error(&state, message, entered(), None);
    }
    match state.add_commands(&names, url) {
        Ok(()) => redirect_with("added", &names.join(", ")),
        Err(WriteError::Rejected(message)) => rerender_error(&state, message, entered(), None),
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

#[derive(Deserialize)]
struct EditCommand {
    /// The entry's current URL, identifying which alias group is being edited.
    original_url: String,
    /// The new alias set (comma- and/or whitespace-separated).
    names: String,
    url: String,
}

/// Save an edit to one entry: replace its alias set and/or its URL.
async fn handle_edit_command(
    State(state): State<Arc<AppState>>,
    Form(form): Form<EditCommand>,
) -> Response {
    let original_url = form.original_url.trim();
    let raw_names = form.names.trim();
    let url = form.url.trim();
    let names = parse_names(raw_names);
    // On rejection, keep this row in edit mode and refill it with what the user
    // typed, so the correction starts from their input rather than reverting to
    // the stored values. The row is still identified by its stored URL.
    let reopen = || EditTarget {
        url: original_url.to_string(),
        values: Some(FormValues { names: raw_names.to_string(), url: url.to_string() }),
    };

    if let Err(message) = validate_names_and_url(&names, url) {
        return rerender_error(&state, message, FormValues::default(), Some(reopen()));
    }
    match state.edit_group(original_url, &names, url) {
        Ok(()) => redirect_with("updated", &names.join(", ")),
        Err(WriteError::Rejected(message)) => {
            rerender_error(&state, message, FormValues::default(), Some(reopen()))
        }
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

#[derive(Deserialize)]
struct DeleteCommand {
    /// The entry's URL, carried as a hidden field so Delete can target the same
    /// alias group the row's Save edits.
    original_url: String,
}

/// Delete one entry — every alias that points at its URL.
async fn handle_delete_command(
    State(state): State<Arc<AppState>>,
    Form(form): Form<DeleteCommand>,
) -> Response {
    let original_url = form.original_url.trim();
    match state.delete_group(original_url) {
        Ok(names) => redirect_with("removed", &names.join(", ")),
        Err(WriteError::Rejected(message)) => {
            rerender_error(&state, message, FormValues::default(), None)
        }
        Err(WriteError::Internal(err)) => internal_error(err),
    }
}

/// Post/redirect/get back to the listing with a confirmation for `action`.
fn redirect_with(action: &str, names: &str) -> Response {
    let names = utf8_percent_encode(names, NON_ALPHANUMERIC);
    Redirect::to(&format!("/commands?{action}={names}")).into_response()
}

/// Re-render the listing (HTTP 400) with an error banner. `add_form` preserves
/// any add-form input the user had typed; `editing` reopens the row that failed
/// to save (with the rejected input), so a failed edit doesn't drop the user
/// out of the editor.
fn rerender_error(
    state: &AppState,
    message: String,
    add_form: FormValues,
    editing: Option<EditTarget>,
) -> Response {
    match load_config(state) {
        Ok(config) => {
            let notice = Notice { error: true, message };
            (
                StatusCode::BAD_REQUEST,
                Html(render_commands_page(
                    &config,
                    Some(&notice),
                    &add_form,
                    editing.as_ref(),
                )),
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

/// Values to pre-fill a name/URL form with — empty on a normal view, the
/// rejected input when re-rendering after a validation error. Used for both the
/// add form and (via [`EditTarget`]) an in-progress edit.
#[derive(Default)]
struct FormValues {
    names: String,
    url: String,
}

/// The single entry the listing should render in edit mode, if any. Identified
/// by its stored (config) URL. `values` is `None` for a freshly opened editor
/// (prefill from the entry's current names/URL) or `Some` when re-rendering
/// after a rejected save (prefill from the user's input so they can correct it).
struct EditTarget {
    url: String,
    values: Option<FormValues>,
}

fn render_commands_page(
    config: &Config,
    notice: Option<&Notice>,
    add_form: &FormValues,
    editing: Option<&EditTarget>,
) -> String {
    // The command list shows one row per entry (a URL with all its aliases).
    // A row is read-only by default — names and URL as plain text with an Edit
    // button — and becomes a form only for the single entry the user chose to
    // edit, identified by its stored URL. So the inputs and the Save/Delete
    // controls that mutate state appear only during an explicit edit, never on
    // every row. Edit is a GET link (`?edit=<url>`); Save posts to
    // /commands/edit and Delete reuses the same form via `formaction` (and
    // `formnovalidate`, so the required fields don't block a delete); Cancel
    // links back to the plain listing. The hidden `original_url` identifies
    // which alias group the edit replaces or the delete removes.
    let mut rows = String::new();
    for group in config.command_groups() {
        let url = escape_html(&group.url);
        let editing_this = editing.is_some_and(|target| target.url == group.url);
        if editing_this {
            let typed = editing.and_then(|target| target.values.as_ref());
            let names = match typed {
                Some(values) => escape_html(&values.names),
                None => escape_html(&group.names.join(", ")),
            };
            let value_url = match typed {
                Some(values) => escape_html(&values.url),
                None => url.clone(),
            };
            rows.push_str(&format!(
                r#"<form class="row editing" id="edit" method="post" action="/commands/edit">
  <input type="hidden" name="original_url" value="{url}">
  <input name="names" value="{names}" aria-label="command name(s)" required autofocus>
  <input name="url" value="{value_url}" aria-label="URL" required>
  <div class="actions">
    <button type="submit" class="btn btn--primary">Save</button>
    <button type="submit" class="btn danger" formaction="/commands/delete" formnovalidate
      onclick="return confirm('Delete this entry?')">Delete</button>
    <a class="btn" href="/commands">Cancel</a>
  </div>
</form>
"#
            ));
        } else {
            let names = escape_html(&group.names.join(", "));
            let edit_query = utf8_percent_encode(&group.url, NON_ALPHANUMERIC);
            rows.push_str(&format!(
                r#"<div class="row">
  <span class="cell-names">{names}</span>
  <span class="cell-url">{url}</span>
  <div class="actions">
    <a class="btn" href="/commands?edit={edit_query}#edit">Edit</a>
  </div>
</div>
"#
            ));
        }
    }

    // The command list is the datasheet: a labeled column header over the rows.
    // Empty state is explicit, not blank.
    let table = if config.commands.is_empty() {
        "<p class=\"empty\">No commands yet.</p>\n".to_string()
    } else {
        format!(
            "<div class=\"cmd-head\"><span>Command(s)</span><span>URL</span><span class=\"act-col\">Actions</span></div>\n<div class=\"rows\">\n{rows}</div>\n"
        )
    };

    let notice_html = match notice {
        Some(notice) => {
            let class = if notice.error { "notice error" } else { "notice ok" };
            format!("<p class=\"{class}\">{}</p>\n", escape_html(&notice.message))
        }
        None => String::new(),
    };

    let form_html = format!(
        r#"<form class="add" method="post" action="/commands">
  <label class="field"><span class="field-label">Name(s)</span>
    <input name="names" placeholder="mail, m" value="{names}" required autofocus></label>
  <label class="field field--grow"><span class="field-label">URL</span>
    <input name="url" placeholder="https://…  (use {{query}} for arguments)" value="{url}" required></label>
  <button type="submit" class="btn btn--primary">Add</button>
</form>
"#,
        names = escape_html(&add_form.names),
        url = escape_html(&add_form.url),
    );

    // Surface the note-capture keyword (configured in the TOML, not via this UI)
    // so it's discoverable alongside the redirect commands.
    let capture_hint = match &config.capture {
        Some(c) => format!(
            " &middot; Capture <code>b {} &lt;text&gt;</code>",
            escape_html(&c.keyword),
        ),
        None => String::new(),
    };

    // Styled after DG-001 (U.S. Graphics school): light "paper + ink", mono,
    // hairline rules, flat fills, sharp corners, single amber signal accent.
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ferry commands</title>
<link rel="search" type="application/opensearchdescription+xml" href="/opensearch.xml" title="ferry">
<style>
  :root {{
    --font-mono: "Berkeley Mono", "JetBrains Mono", "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    --bg: #f4f3ee; --surface: #fafaf8; --ink: #1a1a1a; --ink-muted: #5a584f; --ink-faint: #8a877c;
    --rule: #d2d0c8; --rule-strong: #b4b1a7; --accent: #e8590c; --danger: #c92a2a;
    --s1: 4px; --s2: 8px; --s3: 12px; --s4: 16px; --s5: 24px; --s6: 32px;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    font: 14px/1.45 var(--font-mono); background: var(--bg); color: var(--ink);
    max-width: 64rem; margin: 0 auto; padding: var(--s5) var(--s4) var(--s6);
    -webkit-font-smoothing: antialiased;
  }}
  code {{ background: var(--surface); border: 1px solid var(--rule); padding: 0 var(--s1); }}
  .docbar {{
    display: flex; gap: var(--s4); padding-bottom: var(--s2); margin-bottom: var(--s4);
    border-bottom: 1px solid var(--rule); font-size: 10px; letter-spacing: 1px;
    text-transform: uppercase; color: var(--ink-faint);
  }}
  .docbar .spacer {{ margin-left: auto; }}
  .masthead {{
    display: flex; align-items: baseline; justify-content: space-between; gap: var(--s3);
    padding-bottom: var(--s3); margin-bottom: var(--s5); border-bottom: 1px solid var(--rule-strong);
  }}
  .masthead h1 {{ font-size: 22px; font-weight: 700; letter-spacing: 2px; text-transform: uppercase; margin: 0; }}
  .masthead .note {{ font-size: 11px; color: var(--ink-muted); }}
  .notice {{
    padding: var(--s2) var(--s3); margin: 0 0 var(--s4);
    border: 1px solid var(--rule-strong); font-size: 13px;
  }}
  .notice.ok {{ border-left: 3px solid var(--accent); }}
  .notice.error {{ border-left: 3px solid var(--danger); color: var(--danger); }}
  .panel {{ background: var(--surface); border: 1px solid var(--rule); padding: var(--s4); margin-bottom: var(--s4); }}
  .panel-head {{
    font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 1.5px;
    color: var(--ink-muted); margin: 0 0 var(--s3); padding-bottom: var(--s2); border-bottom: 1px solid var(--rule);
  }}
  input {{
    font: inherit; padding: var(--s2); border: 1px solid var(--rule-strong);
    background: var(--bg); color: var(--ink); border-radius: 0;
  }}
  input:focus {{ outline: 2px solid var(--accent); outline-offset: -1px; }}
  .btn {{
    font: inherit; font-size: 13px; padding: var(--s2) var(--s3); cursor: pointer;
    border: 1px solid var(--rule-strong); background: var(--surface); color: var(--ink);
    border-radius: 0; text-transform: uppercase; letter-spacing: 0.5px; white-space: nowrap;
    display: inline-flex; align-items: center; justify-content: center; text-decoration: none;
  }}
  .btn:hover {{ border-color: var(--ink); }}
  .btn--primary {{ background: var(--accent); border-color: var(--accent); color: #fff; font-weight: 600; }}
  .btn--primary:hover {{ background: #d24f08; border-color: #d24f08; }}
  .btn.danger {{ color: var(--danger); }}
  .btn.danger:hover {{ border-color: var(--danger); }}
  form.add {{ display: flex; gap: var(--s3); align-items: flex-end; flex-wrap: wrap; }}
  .field {{ display: flex; flex-direction: column; gap: var(--s1); }}
  .field--grow {{ flex: 1 1 22rem; }}
  .field-label {{ font-size: 10px; text-transform: uppercase; letter-spacing: 1px; color: var(--ink-muted); }}
  .field input {{ width: 100%; }}
  .cmd-head {{
    display: grid; grid-template-columns: 12rem 1fr auto; gap: var(--s2);
    padding: 0 0 var(--s2); border-bottom: 1px solid var(--rule-strong);
    font-size: 10px; font-weight: 700; text-transform: uppercase; letter-spacing: 1px; color: var(--ink-muted);
  }}
  .cmd-head .act-col {{ text-align: right; }}
  .row {{
    display: grid; grid-template-columns: 12rem 1fr auto; gap: var(--s2);
    align-items: center; padding: var(--s2) 0; border-bottom: 1px solid var(--rule); margin: 0;
  }}
  .rows .row:last-child {{ border-bottom: none; }}
  .row input {{ width: 100%; }}
  .cell-names {{ word-break: break-word; }}
  .cell-url {{ color: var(--ink-muted); word-break: break-all; }}
  .actions {{ display: flex; gap: var(--s2); justify-content: flex-end; }}
  .empty {{ color: var(--ink-faint); font-size: 13px; text-transform: uppercase; letter-spacing: 0.5px; margin: 0; }}
  .fallback {{
    margin: var(--s5) 0 0; padding-top: var(--s3); border-top: 1px solid var(--rule-strong);
    color: var(--ink-muted); font-size: 12px;
  }}
  @media (max-width: 640px) {{
    .cmd-head {{ display: none; }}
    .row {{ grid-template-columns: 1fr; }}
    .actions {{ justify-content: flex-start; }}
  }}
</style>
</head>
<body>
<div class="docbar"><span>DOC. FRY-001</span><span>Address-bar redirector</span><span class="spacer">{count} command(s)</span></div>
<header class="masthead"><h1>ferry</h1><span class="note">type <code>b &lt;cmd&gt;</code> in the address bar</span></header>
{notice_html}<section class="panel">
<h2 class="panel-head">Add command</h2>
{form_html}</section>
<section class="panel">
<h2 class="panel-head">Commands</h2>
{table}</section>
<p class="fallback">Built-in <code>:3000</code> &rarr; <code>http://localhost:3000</code> &middot; Fallback <code>{fallback}</code>{capture_hint}</p>
</body>
</html>
"#,
        count = config.commands.len(),
        fallback = escape_html(&config.fallback),
    )
}

/// A confirmation page for a note capture. `error` is `None` on success, or the
/// reason on failure — the note text is shown either way, so a failed capture
/// can still be copied out rather than lost. Styled to match the DG-001 commands
/// page (paper + ink, mono, hairline rules, single amber accent).
fn render_capture_page(error: Option<&str>, text: &str, open: &str) -> String {
    let open = escape_html(open);
    let text = escape_html(text);
    let (accent_var, heading, detail) = match error {
        None => ("--accent", "Captured", String::new()),
        Some(why) => (
            "--danger",
            "Capture failed",
            format!("<p class=\"why\">{}</p>\n", escape_html(why)),
        ),
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ferry &mdash; {heading}</title>
<style>
  :root {{
    --font-mono: "Berkeley Mono", "JetBrains Mono", "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    --bg: #f4f3ee; --surface: #fafaf8; --ink: #1a1a1a; --ink-muted: #5a584f;
    --rule: #d2d0c8; --rule-strong: #b4b1a7; --accent: #e8590c; --danger: #c92a2a;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    font: 14px/1.5 var(--font-mono); background: var(--bg); color: var(--ink);
    max-width: 40rem; margin: 0 auto; padding: 24px 16px 32px; -webkit-font-smoothing: antialiased;
  }}
  h1 {{
    font-size: 18px; font-weight: 700; letter-spacing: 1.5px; text-transform: uppercase;
    margin: 0 0 16px; padding-bottom: 12px; border-bottom: 2px solid var({accent_var});
  }}
  .why {{ color: var(--danger); margin: 0 0 12px; }}
  blockquote {{
    margin: 0 0 20px; padding: 12px; background: var(--surface);
    border: 1px solid var(--rule); border-left: 3px solid var({accent_var});
    white-space: pre-wrap; word-break: break-word;
  }}
  a {{
    display: inline-block; padding: 8px 12px; border: 1px solid var(--rule-strong);
    background: var(--surface); color: var(--ink); text-decoration: none;
    text-transform: uppercase; letter-spacing: 0.5px; font-size: 13px;
  }}
  a:hover {{ border-color: var(--ink); }}
</style>
</head>
<body>
<h1>{heading}</h1>
{detail}<blockquote>{text}</blockquote>
<a href="{open}">View notes &rarr;</a>
</body>
</html>
"#
    )
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
