//! Federated search: fan a query out to every configured upstream in parallel,
//! adapt each one's response into a common [`Hit`] shape, and return the results
//! grouped by source. A slow or unreachable upstream is isolated — it comes back
//! as a group with an `error` set, never failing the whole search — so code
//! results still show when the notes service is down, and vice-versa.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{Config, SourceConfig, SourceKind};

/// Per-source timeout. An upstream slower than this is reported as an error for
/// that group rather than holding up the rest of the results.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(8);

/// Most hits returned per source — enough to be useful, bounded so one noisy
/// source can't bury the others or bloat the payload.
const DISPLAY_LIMIT: usize = 40;

/// The federated result: the query echoed back, plus one group per source.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub sources: Vec<SourceResult>,
}

/// One source's contribution to the results.
#[derive(Debug, Serialize)]
pub struct SourceResult {
    pub name: String,
    pub label: String,
    /// `code` | `notes` — lets the UI render each group appropriately.
    pub kind: &'static str,
    pub hits: Vec<Hit>,
    /// True when the source had more matches than [`DISPLAY_LIMIT`], or said so.
    pub truncated: bool,
    /// Set when this source couldn't be queried (down, slow, or malformed).
    /// The other groups are unaffected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One normalized match, whatever the source.
#[derive(Debug, Serialize)]
pub struct Hit {
    /// Code: `repo/path`. Notes: empty (the date + snippet carry it).
    pub title: String,
    /// Code: the matching line number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The matching text (a code line, or a note snippet).
    pub snippet: String,
    /// Byte spans into `snippet` to highlight.
    pub ranges: Vec<Range>,
    /// A link to view the match (a GitHub blob for code); absent when none exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Notes: the thought's creation time (epoch ms), for the UI to date it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Range {
    pub start: usize,
    pub len: usize,
}

/// Shared HTTP/search state.
pub struct Engine {
    client: reqwest::Client,
    config: Config,
}

impl Engine {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(SOURCE_TIMEOUT)
            .build()?;
        Ok(Self { client, config })
    }

    /// Run a query across every source in parallel. An empty/whitespace query
    /// short-circuits to empty groups (no upstream calls).
    pub async fn search(self: &Arc<Self>, query: &str) -> SearchResponse {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return SearchResponse { query, sources: Vec::new() };
        }

        // Spawn one task per source so they run concurrently; await in config
        // order so the groups render in a stable order regardless of who's fast.
        let mut handles = Vec::with_capacity(self.config.sources.len());
        for source in &self.config.sources {
            let engine = Arc::clone(self);
            let source = source.clone();
            let query = query.clone();
            handles.push(tokio::spawn(
                async move { engine.fetch(&source, &query).await },
            ));
        }

        let mut sources = Vec::with_capacity(handles.len());
        for (handle, cfg) in handles.into_iter().zip(&self.config.sources) {
            sources.push(handle.await.unwrap_or_else(|join_err| {
                SourceResult::failed(cfg, format!("search task failed: {join_err}"))
            }));
        }
        SearchResponse { query, sources }
    }

    /// Query one source and adapt its response, capturing any failure into the
    /// group's `error` rather than propagating it.
    async fn fetch(&self, source: &SourceConfig, query: &str) -> SourceResult {
        match source.kind {
            SourceKind::Code => self.fetch_code(source, query).await,
            SourceKind::Notes => self.fetch_notes(source, query).await,
            SourceKind::Tickets => self.fetch_tickets(source, query).await,
            SourceKind::Docs => fetch_docs(source, query).await,
        }
        .unwrap_or_else(|err| SourceResult::failed(source, err))
    }

    /// The `source` service: ripgrep results grouped repo → file → match. Flatten
    /// into hits, each linking to the file+line on GitHub.
    async fn fetch_code(&self, source: &SourceConfig, query: &str) -> Result<SourceResult, String> {
        let url = format!("{}/api/search", fetch_base(source)?);
        let resp: CodeResponse = self
            .client
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("upstream error: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bad response: {e}"))?;

        let mut hits = Vec::new();
        for repo in &resp.repos {
            for file in &repo.files {
                for m in &file.matches {
                    hits.push(Hit {
                        title: format!("{}/{}", repo.repo, file.path),
                        line: Some(m.line_number),
                        snippet: m.text.clone(),
                        ranges: spans_from_pairs(&m.ranges),
                        url: Some(github_blob(
                            &self.config.github_org,
                            &repo.repo,
                            &file.path,
                            m.line_number,
                        )),
                        at: None,
                    });
                }
            }
        }
        let truncated = resp.truncated || hits.len() > DISPLAY_LIMIT;
        hits.truncate(DISPLAY_LIMIT);
        Ok(SourceResult { hits, truncated, ..SourceResult::base(source) })
    }

    /// The `lagoon` service: an array of thought matches. Each becomes a dated
    /// snippet hit (lagoon has no per-note URL, so no link).
    async fn fetch_notes(&self, source: &SourceConfig, query: &str) -> Result<SourceResult, String> {
        let url = format!("{}/api/search", fetch_base(source)?);
        let matches: Vec<NoteMatch> = self
            .client
            .get(&url)
            .query(&[("q", query), ("limit", &DISPLAY_LIMIT.to_string())])
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("upstream error: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bad response: {e}"))?;

        let truncated = matches.len() > DISPLAY_LIMIT;
        let hits = matches
            .into_iter()
            .take(DISPLAY_LIMIT)
            .map(|m| Hit {
                title: String::new(),
                line: None,
                snippet: m.snippet,
                ranges: m.ranges.into_iter().map(|r| Range { start: r.start, len: r.len }).collect(),
                url: None,
                at: Some(m.thought.created_at),
            })
            .collect();
        Ok(SourceResult { hits, truncated, ..SourceResult::base(source) })
    }

    /// The `drydock` service: tickets matching the query's text. Each links to the
    /// ticket on the drydock web UI (`?t=<id>`).
    async fn fetch_tickets(&self, source: &SourceConfig, query: &str) -> Result<SourceResult, String> {
        let url = format!("{}/api/tickets", fetch_base(source)?);
        let tickets: Vec<TicketMatch> = self
            .client
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("upstream error: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bad response: {e}"))?;

        let web = link_base(source);
        let truncated = tickets.len() > DISPLAY_LIMIT;
        let hits = tickets
            .into_iter()
            .take(DISPLAY_LIMIT)
            .map(|t| Hit {
                title: format!("#{} {}", t.id, t.title),
                line: None,
                ranges: substring_spans(&t.goal, query),
                snippet: t.goal,
                url: Some(format!("{}/?t={}", web, t.id)),
                at: None,
            })
            .collect();
        Ok(SourceResult { hits, truncated, ..SourceResult::base(source) })
    }
}

/// The `pilot` docs site: match the query against its published service catalog
/// (`fleet.json`) and guidance index (`guidance.json`), read from disk (pilot is
/// process-less static files on the same box). Each hit deep-links into the docs
/// site. Disk-backed, so this is a free function — it needs no HTTP client.
async fn fetch_docs(source: &SourceConfig, query: &str) -> Result<SourceResult, String> {
    let dir = source
        .path
        .as_ref()
        .ok_or_else(|| "docs source needs a `path`".to_string())?;
    let web = link_base(source);
    let needle = query.to_ascii_lowercase();
    let mut hits = Vec::new();
    let mut read_any = false;

    // fleet.json — the service catalog: match name or description.
    if let Ok(text) = tokio::fs::read_to_string(dir.join("fleet.json")).await {
        read_any = true;
        if let Ok(fleet) = serde_json::from_str::<FleetFile>(&text) {
            for svc in fleet.services {
                if svc.name.to_ascii_lowercase().contains(&needle)
                    || svc.description.to_ascii_lowercase().contains(&needle)
                {
                    hits.push(Hit {
                        title: svc.name.clone(),
                        line: None,
                        ranges: substring_spans(&svc.description, query),
                        snippet: svc.description,
                        url: Some(format!("{web}/{}", svc.name)),
                        at: None,
                    });
                }
            }
        }
    }

    // guidance.json — the doc index: match label or category.
    if let Ok(text) = tokio::fs::read_to_string(dir.join("guidance.json")).await {
        read_any = true;
        if let Ok(guidance) = serde_json::from_str::<GuidanceFile>(&text) {
            for doc in guidance.docs {
                let hay = format!("{} {}", doc.label, doc.category).to_ascii_lowercase();
                if hay.contains(&needle) {
                    let snippet = format!("guidance · {}", doc.category);
                    hits.push(Hit {
                        ranges: substring_spans(&snippet, query),
                        title: doc.label,
                        line: None,
                        snippet,
                        url: Some(format!("{web}/guidance/{}", doc.id)),
                        at: None,
                    });
                }
            }
        }
    }

    if !read_any {
        return Err(format!(
            "could not read fleet.json/guidance.json under {}",
            dir.display()
        ));
    }
    let truncated = hits.len() > DISPLAY_LIMIT;
    hits.truncate(DISPLAY_LIMIT);
    Ok(SourceResult { hits, truncated, ..SourceResult::base(source) })
}

/// The base URL to fetch from, trimmed of a trailing slash — required for the
/// HTTP-backed sources.
fn fetch_base(source: &SourceConfig) -> Result<&str, String> {
    source
        .url
        .as_deref()
        .map(|u| u.trim_end_matches('/'))
        .ok_or_else(|| format!("source `{}` needs a `url`", source.name))
}

/// The public base for building result links: `web_url` if set, else the fetch
/// `url`, else empty — both trimmed of a trailing slash.
fn link_base(source: &SourceConfig) -> &str {
    source
        .web_url
        .as_deref()
        .or(source.url.as_deref())
        .unwrap_or("")
        .trim_end_matches('/')
}

/// Case-insensitive byte-offset spans of `needle` within `haystack`, for the
/// frontend to highlight. ASCII-only case folding keeps the byte offsets aligned
/// with the original string (so multi-byte text never misplaces a highlight);
/// a non-ASCII needle just matches case-sensitively. Empty when not found.
fn substring_spans(haystack: &str, needle: &str) -> Vec<Range> {
    if needle.is_empty() {
        return Vec::new();
    }
    let hay = haystack.to_ascii_lowercase();
    let need = needle.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(rel) = hay[from..].find(&need) {
        let start = from + rel;
        spans.push(Range { start, len: need.len() });
        from = start + need.len();
    }
    spans
}

impl SourceResult {
    /// An empty group for `source`, ready to fill in `hits`/`truncated`.
    fn base(source: &SourceConfig) -> Self {
        SourceResult {
            name: source.name.clone(),
            label: source.label(),
            kind: match source.kind {
                SourceKind::Code => "code",
                SourceKind::Notes => "notes",
                SourceKind::Tickets => "tickets",
                SourceKind::Docs => "docs",
            },
            hits: Vec::new(),
            truncated: false,
            error: None,
        }
    }

    /// A group that failed to load, carrying the reason.
    fn failed(source: &SourceConfig, error: String) -> Self {
        SourceResult { error: Some(error), ..Self::base(source) }
    }
}

/// Convert ripgrep's `[start, end]` byte-offset pairs into `{start, len}` spans,
/// dropping any malformed (end before start) pair.
fn spans_from_pairs(pairs: &[[usize; 2]]) -> Vec<Range> {
    pairs
        .iter()
        .filter(|[start, end]| end >= start)
        .map(|[start, end]| Range { start: *start, len: end - start })
        .collect()
}

/// A GitHub blob link to a file at a line, using `HEAD` so it resolves to the
/// repo's default branch without hardcoding `main`.
fn github_blob(org: &str, repo: &str, path: &str, line: u32) -> String {
    format!("https://github.com/{org}/{repo}/blob/HEAD/{path}#L{line}")
}

// --- Upstream response shapes (only the fields we consume) ------------------

#[derive(Debug, Deserialize)]
struct CodeResponse {
    #[serde(default)]
    truncated: bool,
    repos: Vec<CodeRepo>,
}

#[derive(Debug, Deserialize)]
struct CodeRepo {
    repo: String,
    files: Vec<CodeFile>,
}

#[derive(Debug, Deserialize)]
struct CodeFile {
    path: String,
    matches: Vec<CodeMatch>,
}

#[derive(Debug, Deserialize)]
struct CodeMatch {
    line_number: u32,
    text: String,
    /// `[start, end]` byte offsets into `text`.
    ranges: Vec<[usize; 2]>,
}

#[derive(Debug, Deserialize)]
struct NoteMatch {
    thought: NoteThought,
    snippet: String,
    ranges: Vec<NoteRange>,
}

#[derive(Debug, Deserialize)]
struct NoteThought {
    created_at: i64,
}

#[derive(Debug, Deserialize)]
struct NoteRange {
    start: usize,
    len: usize,
}

#[derive(Debug, Deserialize)]
struct TicketMatch {
    id: i64,
    title: String,
    goal: String,
}

#[derive(Debug, Deserialize)]
struct FleetFile {
    services: Vec<ServiceDoc>,
}

#[derive(Debug, Deserialize)]
struct ServiceDoc {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct GuidanceFile {
    docs: Vec<GuidanceItem>,
}

#[derive(Debug, Deserialize)]
struct GuidanceItem {
    id: String,
    label: String,
    #[serde(default)]
    category: String,
}

#[cfg(test)]
mod tests {
    use super::{github_blob, spans_from_pairs, substring_spans};

    #[test]
    fn finds_case_insensitive_substring_spans() {
        // Two matches, case-insensitive, with byte offsets into the original.
        let spans = substring_spans("Tide Theme tide", "tide");
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].len), (0, 4));
        assert_eq!((spans[1].start, spans[1].len), (11, 4));
        // No match, and an empty needle, both yield nothing.
        assert!(substring_spans("nothing here", "xyz").is_empty());
        assert!(substring_spans("anything", "").is_empty());
    }

    #[test]
    fn converts_offset_pairs_to_spans() {
        let spans = spans_from_pairs(&[[6, 13], [0, 7]]);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].len), (6, 7));
        assert_eq!((spans[1].start, spans[1].len), (0, 7));
        // A zero-width or reversed pair is dropped, not panicked on.
        assert_eq!(spans_from_pairs(&[[5, 5]]).len(), 1); // zero-width kept (len 0)
        assert!(spans_from_pairs(&[[9, 3]]).is_empty()); // reversed dropped
    }

    #[test]
    fn builds_github_blob_link_at_line() {
        assert_eq!(
            github_blob("deepwa7er", "ferry", "src/web.rs", 42),
            "https://github.com/deepwa7er/ferry/blob/HEAD/src/web.rs#L42",
        );
    }
}
