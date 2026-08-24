//! OpenCode's native HTTP and event-stream adapter.
//!
//! OpenCode owns its sessions behind `opencode serve`; there is no directory
//! for Skiff to tail. The client below accepts only a loopback base URL, maps
//! raw server shapes into Skiff's domain, and leaves persistence and topic
//! invalidation to [`OpencodeIngest`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::{Method, StatusCode, Url};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::content::parse;
use crate::model::{
    Capabilities, Entry, Harness, Message, Part, Role, SessionKey, SessionSummary, SourceHealth,
    ToolStatus,
};
use crate::run::opencode::OpencodeRuns;
use crate::store::{SessionIngest, Store};

use super::Topic;

pub const SOURCE: &str = "opencode";
pub const DEFAULT_URL: &str = "http://127.0.0.1:4130";
pub const CAPABILITIES: Capabilities = Capabilities {
    rename: true,
    orchestrator: false,
    model: false,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const FLOOR_SCAN: Duration = Duration::from_secs(15);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct OpencodeClient {
    base: Url,
    http: reqwest::Client,
}

impl OpencodeClient {
    pub fn new(base: &str) -> Result<Self, String> {
        let mut base =
            Url::parse(base).map_err(|error| format!("invalid OpenCode URL: {error}"))?;
        if base.scheme() != "http" {
            return Err("OpenCode serve must use loopback HTTP".to_owned());
        }
        let loopback = match base.host_str() {
            Some("localhost") => true,
            Some(host) => host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()),
            None => false,
        };
        if !loopback {
            return Err(format!("OpenCode serve must be loopback-only (got {base})"));
        }
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        Ok(Self {
            base,
            http: reqwest::Client::new(),
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        body: Option<Value>,
        allow_missing: bool,
    ) -> Result<Option<Value>> {
        let url = self.endpoint(path)?;
        let mut request = self.http.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, request.send())
            .await
            .context("OpenCode serve timed out")?
            .context("OpenCode serve unreachable")?;
        if allow_missing && response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            let status = response.status();
            let snippet = response.text().await.unwrap_or_default();
            let snippet: String = snippet.chars().take(120).collect();
            bail!(
                "OpenCode serve answered HTTP {status}{}",
                if snippet.is_empty() {
                    String::new()
                } else {
                    format!(": {snippet}")
                }
            );
        }
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        Ok(Some(
            response
                .json()
                .await
                .context("OpenCode serve sent invalid JSON")?,
        ))
    }

    pub async fn sessions(&self) -> Result<Vec<Value>> {
        let value = self
            .request(Method::GET, &["session"], None, false)
            .await?
            .context("OpenCode session list had no body")?;
        value
            .as_array()
            .cloned()
            .context("OpenCode session list was not an array")
    }

    pub async fn session(&self, id: &str) -> Result<Option<Value>> {
        self.request(Method::GET, &["session", id], None, true)
            .await
    }

    pub async fn messages(&self, id: &str) -> Result<Vec<Value>> {
        let value = self
            .request(Method::GET, &["session", id, "message"], None, false)
            .await?
            .context("OpenCode messages had no body")?;
        value
            .as_array()
            .cloned()
            .context("OpenCode messages were not an array")
    }

    pub async fn prompt(&self, id: &str, text: &str) -> Result<()> {
        self.request(
            Method::POST,
            &["session", id, "prompt_async"],
            Some(json!({ "parts": [{ "type": "text", "text": text }] })),
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn abort(&self, id: &str) -> Result<()> {
        self.request(Method::POST, &["session", id, "abort"], None, false)
            .await?;
        Ok(())
    }

    pub async fn rename(&self, id: &str, title: &str) -> Result<()> {
        self.request(
            Method::PATCH,
            &["session", id],
            Some(json!({ "title": title })),
            false,
        )
        .await?;
        Ok(())
    }

    async fn events(&self) -> Result<reqwest::Response> {
        let url = self.endpoint(&["event"])?;
        let response = tokio::time::timeout(REQUEST_TIMEOUT, self.http.get(url).send())
            .await
            .context("OpenCode event stream timed out")?
            .context("opening OpenCode event stream")?;
        if !response.status().is_success() {
            bail!("OpenCode event stream answered HTTP {}", response.status());
        }
        Ok(response)
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("OpenCode URL cannot hold path segments"))?
            .clear()
            .extend(segments);
        Ok(url)
    }
}

struct Snapshot {
    summary: SessionSummary,
    entries: Vec<Entry>,
    pending: Option<Message>,
    user_count: usize,
}

#[derive(Default)]
struct Invalidation {
    all: bool,
    sessions: HashSet<String>,
}

impl Invalidation {
    fn is_empty(&self) -> bool {
        !self.all && self.sessions.is_empty()
    }

    fn clear(&mut self) {
        self.all = false;
        self.sessions.clear();
    }
}

pub struct OpencodeIngest {
    client: Result<OpencodeClient, String>,
    store: Store,
    runs: Arc<OpencodeRuns>,
    topics: broadcast::Sender<Topic>,
}

impl OpencodeIngest {
    pub fn new(
        client: Result<OpencodeClient, String>,
        store: Store,
        runs: Arc<OpencodeRuns>,
        topics: broadcast::Sender<Topic>,
    ) -> Self {
        Self {
            client,
            store,
            runs,
            topics,
        }
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.pump().await })
    }

    async fn pump(self) {
        let client = match self.client.clone() {
            Ok(client) => client,
            Err(error) => {
                self.record_health(error).await;
                return;
            }
        };
        loop {
            self.scan(&client).await;
            match client.events().await {
                Ok(response) => {
                    self.record_event_health(None).await;
                    self.follow_events(&client, response).await;
                    tokio::time::sleep(RECONNECT_DELAY).await;
                }
                Err(error) => {
                    self.record_event_health(Some(format!("{error:#}"))).await;
                    tokio::time::sleep(RECONNECT_DELAY).await;
                }
            }
        }
    }

    async fn follow_events(&self, client: &OpencodeClient, response: reqwest::Response) {
        let mut stream = response.bytes_stream();
        let mut floor = tokio::time::interval(FLOOR_SCAN);
        floor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        floor.tick().await;
        let mut buffer = Vec::new();
        let mut invalidation = Invalidation::default();
        let debounce = tokio::time::sleep(Duration::from_secs(86_400));
        tokio::pin!(debounce);

        loop {
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(Ok(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                        drain_events(&mut buffer, &mut invalidation);
                        if !invalidation.is_empty() {
                            debounce.as_mut().reset(tokio::time::Instant::now() + EVENT_DEBOUNCE);
                        }
                    }
                    Some(Err(error)) => {
                        self.record_event_health(Some(format!("OpenCode event stream failed: {error}"))).await;
                        return;
                    }
                    None => {
                        self.record_event_health(Some("OpenCode event stream closed".to_owned())).await;
                        return;
                    }
                },
                _ = &mut debounce, if !invalidation.is_empty() => {
                    if invalidation.all {
                        self.scan(client).await;
                    } else {
                        let sessions = invalidation.sessions.drain().collect();
                        self.scan_sessions(client, sessions).await;
                    }
                    invalidation.clear();
                    debounce.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(86_400));
                }
                _ = floor.tick() => self.scan(client).await,
            }
        }
    }

    async fn scan(&self, client: &OpencodeClient) {
        match build_snapshots(client).await {
            Ok(snapshots) => self.apply(snapshots, ScanScope::All).await,
            Err(error) => self.record_health(format!("{error:#}")).await,
        }
    }

    async fn scan_sessions(&self, client: &OpencodeClient, ids: Vec<String>) {
        match build_selected_snapshots(client, ids).await {
            Ok((snapshots, missing)) => self.apply(snapshots, ScanScope::Selected(missing)).await,
            Err(error) => self.record_health(format!("{error:#}")).await,
        }
    }

    async fn apply(&self, snapshots: Vec<Snapshot>, scope: ScanScope) {
        let store = self.store.clone();
        let applied =
            tokio::task::spawn_blocking(move || apply_snapshots(&store, &snapshots, scope))
                .await
                .context("the OpenCode store task panicked")
                .and_then(|result| result);
        match applied {
            Ok((topics, lives, forgotten)) => {
                self.record_health_ok().await;
                for topic in topics {
                    let _ = self.topics.send(topic);
                }
                for (session, pending, users) in lives {
                    self.runs.observed(session, pending, users).await;
                }
                for session in forgotten {
                    self.runs.forgot(&session).await;
                }
            }
            Err(error) => self.record_health(format!("{error:#}")).await,
        }
    }

    async fn record_health_ok(&self) {
        self.set_health(SOURCE, None).await;
    }

    async fn record_health(&self, error: String) {
        self.set_health(SOURCE, Some(error)).await;
    }

    async fn record_event_health(&self, error: Option<String>) {
        self.set_health("opencode events", error).await;
    }

    async fn set_health(&self, source: &str, error: Option<String>) {
        let store = self.store.clone();
        let health = SourceHealth {
            source: source.to_owned(),
            error,
            checked_ms: super::now_ms(),
        };
        let result = tokio::task::spawn_blocking(move || {
            let previous = store
                .source_health()?
                .into_iter()
                .find(|item| item.source == health.source)
                .map(|item| item.error);
            store.set_source_health(&health)?;
            Ok::<_, anyhow::Error>(previous.as_ref() != Some(&health.error))
        })
        .await;
        match result {
            Ok(Ok(true)) => {
                let _ = self.topics.send(Topic::SourceHealth);
            }
            Ok(Ok(false)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "could not record OpenCode health"),
            Err(error) => tracing::warn!(%error, "OpenCode health task panicked"),
        }
    }
}

async fn build_snapshots(client: &OpencodeClient) -> Result<Vec<Snapshot>> {
    let sessions = client.sessions().await?;
    futures_util::stream::iter(sessions)
        .filter(|session| {
            futures_util::future::ready(session.get("parentID").is_none_or(Value::is_null))
        })
        .map(|session| {
            let client = client.clone();
            async move { build_snapshot(&client, session).await }
        })
        .buffer_unordered(8)
        .collect::<Vec<Result<Snapshot>>>()
        .await
        .into_iter()
        .collect()
}

async fn build_selected_snapshots(
    client: &OpencodeClient,
    ids: Vec<String>,
) -> Result<(Vec<Snapshot>, HashSet<SessionKey>)> {
    let requested: HashSet<_> = ids.iter().cloned().collect();
    let results = futures_util::stream::iter(ids)
        .map(|id| {
            let client = client.clone();
            async move {
                let session = client.session(&id).await?;
                match session {
                    Some(session) if session.get("parentID").is_none_or(Value::is_null) => {
                        Ok(Some(build_snapshot(&client, session).await?))
                    }
                    _ => Ok(None),
                }
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<Result<Option<Snapshot>>>>()
        .await;
    let mut snapshots = Vec::new();
    for result in results {
        if let Some(snapshot) = result? {
            snapshots.push(snapshot);
        }
    }
    let present: HashSet<_> = snapshots
        .iter()
        .map(|snapshot| snapshot.summary.id.local.clone())
        .collect();
    let missing = requested
        .difference(&present)
        .map(|id| SessionKey::new(Harness::Opencode, id))
        .collect();
    Ok((snapshots, missing))
}

async fn build_snapshot(client: &OpencodeClient, session: Value) -> Result<Snapshot> {
    let summary = map_session(&session)?;
    let messages = client.messages(&summary.id.local).await?;
    let (entries, pending, user_count) = map_messages(messages);
    Ok(Snapshot {
        summary,
        entries,
        pending,
        user_count,
    })
}

type LiveObservation = (SessionKey, Option<Message>, usize);

enum ScanScope {
    All,
    Selected(HashSet<SessionKey>),
}

fn apply_snapshots(
    store: &Store,
    snapshots: &[Snapshot],
    scope: ScanScope,
) -> Result<(HashSet<Topic>, Vec<LiveObservation>, Vec<SessionKey>)> {
    let mut topics = HashSet::new();
    let seen: HashSet<_> = snapshots
        .iter()
        .map(|snapshot| snapshot.summary.id.clone())
        .collect();
    let existing: HashMap<_, _> = store
        .sessions()?
        .into_iter()
        .filter(|summary| summary.harness == Harness::Opencode)
        .map(|summary| (summary.id.clone(), summary))
        .collect();
    let mut lives = Vec::new();
    let mut forgotten = Vec::new();

    for snapshot in snapshots {
        let changed = existing.get(&snapshot.summary.id) != Some(&snapshot.summary)
            || store.entries(&snapshot.summary.id)? != snapshot.entries;
        if changed {
            store.replace_session(SessionIngest {
                summary: &snapshot.summary,
                state: None,
                entries: &snapshot.entries,
            })?;
            topics.insert(Topic::Session(snapshot.summary.id.clone()));
            topics.insert(Topic::SessionList);
        }
        lives.push((
            snapshot.summary.id.clone(),
            snapshot.pending.clone(),
            snapshot.user_count,
        ));
    }
    let stale: Vec<_> = match scope {
        ScanScope::All => existing
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect(),
        ScanScope::Selected(missing) => missing.into_iter().collect(),
    };
    for stale in stale {
        store.forget_session(&stale)?;
        forgotten.push(stale.clone());
        topics.insert(Topic::Session(stale));
        topics.insert(Topic::SessionList);
    }
    Ok((topics, lives, forgotten))
}

fn map_session(value: &Value) -> Result<SessionSummary> {
    let local = value
        .get("id")
        .and_then(Value::as_str)
        .context("OpenCode session had no id")?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .or_else(|| value.get("slug").and_then(Value::as_str))
        .map(str::to_owned);
    Ok(SessionSummary {
        id: SessionKey::new(Harness::Opencode, local),
        harness: Harness::Opencode,
        capabilities: CAPABILITIES,
        title,
        directory: value
            .get("directory")
            .and_then(Value::as_str)
            .map(str::to_owned),
        created_ms: value
            .get("time")
            .and_then(|time| time.get("created"))
            .and_then(Value::as_i64),
        updated_ms: value
            .get("time")
            .and_then(|time| time.get("updated"))
            .and_then(Value::as_i64),
        model: value
            .get("model")
            .and_then(|model| model.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        orchestrator_active: false,
    })
}

fn map_messages(values: Vec<Value>) -> (Vec<Entry>, Option<Message>, usize) {
    let mut entries = Vec::new();
    let mut pending = None;
    let mut previous = None;
    let mut users = 0;
    for value in values {
        let Some(info) = value.get("info") else {
            continue;
        };
        let Some(id) = info.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let role = match info.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        let completed = info
            .get("time")
            .and_then(|time| time.get("completed"))
            .and_then(Value::as_i64);
        let created = info
            .get("time")
            .and_then(|time| time.get("created"))
            .and_then(Value::as_i64);
        let message = Message {
            id: id.clone(),
            role,
            agent: info
                .get("modelID")
                .and_then(Value::as_str)
                .map(str::to_owned),
            created_ms: created,
            completed_ms: if role == Role::Assistant {
                completed
            } else {
                completed.or(created)
            },
            parts: map_parts(value.get("parts").and_then(Value::as_array)),
        };
        if role == Role::Assistant && completed.is_none() {
            pending = Some(message);
            continue;
        }
        users += usize::from(role == Role::User);
        let seq = entries.len() as i64;
        entries.push(Entry {
            seq,
            id: id.clone(),
            parent_id: previous.clone(),
            raw: value,
            mapped: Some(message),
        });
        previous = Some(id);
    }
    (entries, pending, users)
}

fn map_parts(parts: Option<&Vec<Value>>) -> Vec<Part> {
    parts
        .into_iter()
        .flatten()
        .filter_map(|part| match part.get("type").and_then(Value::as_str)? {
            "text"
                if !part
                    .get("synthetic")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
            {
                Some(Part::Text {
                    blocks: parse(part.get("text").and_then(Value::as_str).unwrap_or_default()),
                })
            }
            "reasoning" => Some(Part::Reasoning {
                blocks: parse(part.get("text").and_then(Value::as_str).unwrap_or_default()),
            }),
            "tool" => {
                let state = part.get("state");
                let status = match state
                    .and_then(|state| state.get("status"))
                    .and_then(Value::as_str)
                {
                    Some("completed") => ToolStatus::Completed,
                    Some("error") => ToolStatus::Error,
                    _ => ToolStatus::Running,
                };
                let output = if status == ToolStatus::Error {
                    state
                        .and_then(|state| state.get("error"))
                        .and_then(Value::as_str)
                } else {
                    state
                        .and_then(|state| state.get("output"))
                        .and_then(Value::as_str)
                };
                Some(Part::Tool {
                    call_id: part
                        .get("callID")
                        .or_else(|| part.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: part
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    status,
                    output: output.map(str::to_owned),
                })
            }
            "file" => Some(Part::File {
                filename: part
                    .get("filename")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_owned(),
            }),
            _ => None,
        })
        .collect()
}

fn drain_events(buffer: &mut Vec<u8>, invalidation: &mut Invalidation) {
    while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer[..newline].to_vec();
        buffer.drain(..=newline);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if let Some(data) = line.strip_prefix(b"data: ")
            && let Ok(event) = serde_json::from_slice::<Value>(data)
        {
            match event
                .get("properties")
                .and_then(|properties| properties.get("sessionID"))
                .and_then(Value::as_str)
            {
                Some(id) => {
                    invalidation.sessions.insert(id.to_owned());
                }
                None => invalidation.all = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_are_rejected() {
        assert!(OpencodeClient::new("https://example.com").is_err());
        assert!(OpencodeClient::new("http://10.0.0.1:4130").is_err());
        assert!(OpencodeClient::new("http://127.0.0.1:4130").is_ok());
    }

    #[test]
    fn streaming_assistant_content_becomes_live_not_committed() {
        let (_, pending, users) = map_messages(vec![json!({
            "info": { "id": "m1", "role": "assistant", "time": { "created": 1 } },
            "parts": [{ "type": "text", "text": "still writing" }]
        })]);
        assert_eq!(users, 0);
        assert_eq!(pending.unwrap().id, "m1");
    }

    #[test]
    fn event_frames_survive_chunk_boundaries() {
        let mut buffer = b"data: {\"type\":\"one\"}".to_vec();
        let mut invalidation = Invalidation::default();
        drain_events(&mut buffer, &mut invalidation);
        assert!(invalidation.is_empty());
        buffer.extend_from_slice(
            b"\n\ndata: {\"type\":\"two\",\"properties\":{\"sessionID\":\"s1\"}}\n",
        );
        drain_events(&mut buffer, &mut invalidation);
        assert!(invalidation.sessions.contains("s1"));
        assert!(buffer.is_empty());
    }
}
