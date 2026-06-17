use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::ExternalLink;
use crate::git;
use crate::github::Activity;
use crate::lighthouse::ServiceHealth;
use crate::topology::Topology;

/// One project or area, as declared by a secondbrain file's frontmatter.
///
/// Mirrors the contract documented in the secondbrain's `SCHEMA.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub stack: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    pub updated: String,

    /// Recent commits (newest first), attached after parsing (not frontmatter).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<Activity>,

    /// Individual items from the note's `## Next` section (one per bullet).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_items: Vec<String>,
}

/// The full snapshot the API serves.
#[derive(Debug, Clone, Serialize)]
pub struct State {
    /// Unix seconds at which this snapshot was built.
    pub generated_at: u64,
    pub source: Source,
    pub projects: Vec<Entry>,
    pub areas: Vec<Entry>,
    /// Live VPS service health from lighthouse (empty if not configured).
    #[serde(default)]
    pub services: Vec<ServiceHealth>,
    /// Persistent launchable links to other tailnet services, from config.
    #[serde(default)]
    pub external_links: Vec<ExternalLink>,
    /// Declared graph topology (machines, placements, edges). `None` when the
    /// secondbrain has no `topology.yaml` — the graph view degrades gracefully.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<Topology>,
    /// Map of entry name → markdown file path, for the note endpoint. Not served.
    #[serde(skip)]
    pub note_paths: HashMap<String, PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub dir: String,
    pub commit: Option<String>,
    pub remote: Option<String>,
}

/// Build a fresh [`State`] by reading every `projects/*.md` and `areas/*.md`
/// file under `source_dir` and parsing its frontmatter.
///
/// A file that fails to parse is skipped with a warning rather than failing the
/// whole snapshot — one malformed file should not blank the dashboard.
pub fn build(source_dir: &Path, remote: Option<&str>) -> Result<State> {
    let mut note_paths = HashMap::new();
    let mut projects = read_dir(&source_dir.join("projects"), &mut note_paths);
    let mut areas = read_dir(&source_dir.join("areas"), &mut note_paths);

    projects.sort_by(|a, b| status_rank(&a.status).cmp(&status_rank(&b.status)).then(a.name.cmp(&b.name)));
    areas.sort_by(|a, b| a.name.cmp(&b.name));

    // Read declared graph topology and prune any references to projects/machines
    // that don't exist, so the served graph only points at real nodes.
    let topology = crate::topology::load(source_dir).map(|mut t| {
        let project_names: HashSet<String> = projects.iter().map(|p| p.name.clone()).collect();
        t.validate(&project_names);
        t
    });

    Ok(State {
        generated_at: now_unix(),
        source: Source {
            dir: source_dir.display().to_string(),
            commit: git::head_commit(source_dir),
            remote: remote.map(str::to_string),
        },
        projects,
        areas,
        services: Vec::new(),
        external_links: Vec::new(),
        topology,
        note_paths,
    })
}

/// Parse every `*.md` file in `dir` into an [`Entry`], skipping unparseable ones.
/// Records each entry's name → file path in `note_paths` for the note endpoint.
fn read_dir(dir: &Path, note_paths: &mut HashMap<String, PathBuf>) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        tracing::warn!(dir = %dir.display(), "directory not found; skipping");
        return Vec::new();
    };

    let mut entries = Vec::new();
    for dirent in read.flatten() {
        let path = dirent.path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        match parse_file(&path) {
            Ok(Some(entry)) => {
                note_paths.insert(entry.name.clone(), path);
                entries.push(entry);
            }
            Ok(None) => tracing::debug!(file = %path.display(), "no frontmatter; skipping"),
            Err(e) => tracing::warn!(file = %path.display(), error = %e, "skipping unparseable file"),
        }
    }
    entries
}

/// Render the markdown body (everything after the frontmatter) of a note file
/// to HTML, for the note detail view.
pub fn render_note(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(render_markdown(strip_frontmatter(&text)))
}

/// Render a markdown string to HTML.
fn render_markdown(md: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(md, options);

    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Extract the bullet items of a `## Next` (or `## Next steps`) section from a
/// note's markdown. Case-insensitive heading; the section runs until the next
/// heading at the same or higher level. Each top-level `-`/`*` bullet becomes
/// one item (marker stripped). Returns an empty vec if there is no such section.
fn next_items(text: &str) -> Vec<String> {
    let body = strip_frontmatter(text);
    let lines: Vec<&str> = body.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let Some((level, title)) = heading(line) else {
            continue;
        };
        let title = title.trim().to_ascii_lowercase();
        if title != "next" && title != "next steps" {
            continue;
        }
        // Collect until the next heading at the same or higher level.
        let mut end = i + 1;
        while end < lines.len() {
            if let Some((lvl, _)) = heading(lines[end]) {
                if lvl <= level {
                    break;
                }
            }
            end += 1;
        }
        return lines[i + 1..end]
            .iter()
            .filter_map(|l| bullet_text(l))
            .collect();
    }
    Vec::new()
}

/// If a line is a top-level markdown bullet (`- ` or `* `), return its text.
fn bullet_text(line: &str) -> Option<String> {
    let t = line.trim_start();
    // Only top-level bullets (not indented sub-items).
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    for marker in ["- ", "* "] {
        if let Some(rest) = t.strip_prefix(marker) {
            let item = rest.trim();
            if !item.is_empty() {
                return Some(item.to_string());
            }
        }
    }
    None
}

/// Parse a markdown ATX heading line into (level, title), if it is one.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some((hashes, line[hashes + 1..].trim()))
    } else {
        None
    }
}

/// The markdown body after a leading frontmatter block (or the whole text if
/// there is none).
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    let Some(idx) = rest.find("\n---") else {
        return text;
    };
    // `rest[idx..]` is "\n---…"; advance past the rest of that closing line.
    let after = &rest[idx + 1..];
    match after.find('\n') {
        Some(nl) => &after[nl + 1..],
        None => "",
    }
}

/// Read a file and parse its leading YAML frontmatter block into an [`Entry`].
/// Returns `Ok(None)` when the file has no frontmatter.
fn parse_file(path: &Path) -> Result<Option<Entry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let Some(frontmatter) = extract_frontmatter(&text) else {
        return Ok(None);
    };
    let mut entry: Entry = serde_yaml::from_str(frontmatter)
        .with_context(|| format!("parsing frontmatter of {}", path.display()))?;
    entry.next_items = next_items(&text);
    Ok(Some(entry))
}

/// Return the text between the leading `---` fence and its closing `---`, or
/// `None` if the document does not open with a frontmatter fence.
fn extract_frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// Display/priority order for statuses, so the most active work sorts first.
fn status_rank(status: &str) -> u8 {
    match status {
        "active" => 0,
        "shipped" => 1,
        "stable" => 2,
        "learning" => 3,
        "idea" => 4,
        "archived" => 5,
        _ => 6,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_frontmatter_block() {
        let doc = "---\nname: x\nstatus: active\n---\n\n# Body\n";
        assert_eq!(extract_frontmatter(doc), Some("name: x\nstatus: active"));
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert_eq!(extract_frontmatter("# Just a heading\n"), None);
    }

    #[test]
    fn parses_full_entry() {
        let doc = "---\nname: sonar\nkind: project\nstatus: active\nsummary: s\nrepo: deepwa7er/sonar\nupdated: 2026-06-13\n---\n# sonar\n";
        let fm = extract_frontmatter(doc).unwrap();
        let entry: Entry = serde_yaml::from_str(fm).unwrap();
        assert_eq!(entry.name, "sonar");
        assert_eq!(entry.repo.as_deref(), Some("deepwa7er/sonar"));
        assert_eq!(entry.phase, None);
    }

    #[test]
    fn null_repo_is_none() {
        let doc = "---\nname: rproxy\nkind: project\nstatus: learning\nsummary: s\nrepo: null\nupdated: 2026-06-13\n---\n";
        let fm = extract_frontmatter(doc).unwrap();
        let entry: Entry = serde_yaml::from_str(fm).unwrap();
        assert_eq!(entry.repo, None);
    }
}
