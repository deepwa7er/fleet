use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Latest-commit activity for a project's repo, shown in the Fleet card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    /// Short commit hash.
    pub sha: String,
    /// ISO-8601 commit timestamp (the extension renders "active 2d ago").
    pub date: String,
    /// First line of the commit message.
    pub message: String,
}

// Only the fields we read from GitHub's commits API.
#[derive(Deserialize)]
struct CommitResp {
    sha: String,
    commit: CommitObj,
}
#[derive(Deserialize)]
struct CommitObj {
    message: String,
    committer: GitActor,
}
#[derive(Deserialize)]
struct GitActor {
    date: String,
}

/// Fetch the most recent `n` commits on the default branch of `repo`
/// ("owner/name"), newest first.
///
/// Returns `Ok(vec![])` when there's nothing to show — no commits, a private
/// repo with no token, a rate-limit response, etc. Activity is best-effort: a
/// missing signal should never blank the dashboard, so non-success is not an
/// error.
pub async fn recent_commits(
    http: &reqwest::Client,
    token: Option<&str>,
    repo: &str,
    n: u32,
) -> Result<Vec<Activity>> {
    let url = format!("https://api.github.com/repos/{repo}/commits?per_page={n}");
    let mut req = http
        .get(&url)
        .header("User-Agent", "harbor")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        tracing::debug!(repo, status = %resp.status(), "no commit activity");
        return Ok(Vec::new());
    }

    let commits: Vec<CommitResp> = resp.json().await.context("parsing commits")?;
    Ok(commits
        .into_iter()
        .map(|c| Activity {
            sha: c.sha.chars().take(7).collect(),
            date: c.commit.committer.date,
            message: c.commit.message.lines().next().unwrap_or("").to_string(),
        })
        .collect())
}
