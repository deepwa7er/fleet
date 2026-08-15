use super::GitRecord;
use anyhow::Context;
use chrono::{DateTime, Utc};
use std::path::Path;

pub fn extract_git(repo_path: &Path, repo_id: &str, limit: usize) -> anyhow::Result<Vec<GitRecord>> {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()), // not a git repo
    };
    let mut revwalk = repo.revwalk().context("revwalk")?;
    revwalk.push_head().ok();
    // also try HEAD if no head
    if revwalk.count() == 0 {
        return Ok(Vec::new());
    }
    let mut revwalk = repo.revwalk().context("revwalk2")?;
    revwalk.push_head().context("push head")?;
    revwalk.set_sorting(git2::Sort::TIME).ok();

    let mut out = Vec::new();
    for oid in revwalk.take(limit) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let ts = commit.time().seconds();
        let dt = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_else(Utc::now);
        let author = commit.author();
        let tree = commit.tree().ok();
        let parent_tree = commit.parents().next().and_then(|p| p.tree().ok());
        let files_changed = match (tree, parent_tree) {
            (Some(t), Some(pt)) => {
                let diff = repo.diff_tree_to_tree(Some(&pt), Some(&t), None).ok();
                diff.map(|d| d.deltas().len() as i32).unwrap_or(0)
            }
            (Some(t), None) => t.len() as i32,
            _ => 0,
        };
        out.push(GitRecord {
            repo_id: repo_id.to_string(),
            commit_hash: oid.to_string(),
            author: author.name().map(|s| s.to_string()),
            author_email: author.email().map(|s| s.to_string()),
            ts: dt,
            message: commit.message().map(|s| s.lines().next().unwrap_or("").to_string()),
            files_changed,
        });
    }
    Ok(out)
}
