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

/// Fetch the latest commit on the default branch of `repo` ("owner/name").
///
/// Returns `Ok(None)` when there's nothing to show — no commits, a private repo
/// with no token, a rate-limit response, etc. Activity is best-effort: a missing
/// signal should never blank the dashboard, so non-success is not an error.
pub async fn latest_commit(
    http: &reqwest::Client,
    token: Option<&str>,
    repo: &str,
) -> Result<Option<Activity>> {
    let url = format!("https://api.github.com/repos/{repo}/commits?per_page=1");
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
        return Ok(None);
    }

    let commits: Vec<CommitResp> = resp.json().await.context("parsing commits")?;
    let Some(commit) = commits.into_iter().next() else {
        return Ok(None);
    };

    Ok(Some(Activity {
        sha: commit.sha.chars().take(7).collect(),
        date: commit.commit.committer.date,
        message: commit.commit.message.lines().next().unwrap_or("").to_string(),
    }))
}
