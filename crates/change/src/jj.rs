//! Typed jj facts and landing primitives.
//!
//! Reads always use `--ignore-working-copy`; mutations participate in jj's
//! normal snapshot and operation-log discipline. No command is executed
//! through a shell.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Error, Result};

const SHOW_TEMPLATE: &str = concat!(
    "change_id ++ \"\\x1f\" ++ commit_id ++ \"\\x1f\" ++ ",
    "description.first_line() ++ \"\\x1f\" ++ author.email() ++ \"\\x1f\" ++ ",
    "committer.timestamp().format(\"%Y-%m-%dT%H:%M:%S%z\") ++ \"\\x1f\" ++ ",
    "parents.map(|c| c.change_id()).join(\",\") ++ \"\\n\"",
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct Commit {
    pub change_id: String,
    pub commit_id: String,
    pub description: String,
    pub author_email: String,
    pub timestamp: String,
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommit {
    pub commit: Option<Commit>,
    pub divergent: bool,
}

#[derive(Debug, Clone)]
pub struct Jj {
    binary: PathBuf,
}

impl Jj {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn show(&self, repo: &Path, change_id: &str) -> Result<ResolvedCommit> {
        require_change_id(change_id)?;
        let output = self.run_read(
            repo,
            &["log", "--no-graph", "-r", change_id, "-T", SHOW_TEMPLATE],
        );
        let stdout = match output {
            Ok(stdout) => stdout,
            Err(Error::Jj(message)) if message.contains("doesn't exist") => {
                return Ok(ResolvedCommit {
                    commit: None,
                    divergent: false,
                });
            }
            Err(error) => return Err(error),
        };
        let records: Vec<_> = stdout.lines().filter(|line| !line.is_empty()).collect();
        if records.is_empty() {
            return Ok(ResolvedCommit {
                commit: None,
                divergent: false,
            });
        }
        if records.len() > 1 {
            return Ok(ResolvedCommit {
                commit: None,
                divergent: true,
            });
        }
        let fields: Vec<_> = records[0].split('\x1f').collect();
        if fields.len() != 6 {
            return Err(Error::Jj(format!(
                "jj returned malformed commit metadata for {change_id}"
            )));
        }
        Ok(ResolvedCommit {
            commit: Some(Commit {
                change_id: fields[0].to_owned(),
                commit_id: fields[1].to_owned(),
                description: fields[2].to_owned(),
                author_email: fields[3].to_owned(),
                timestamp: fields[4].to_owned(),
                parents: if fields[5].is_empty() {
                    Vec::new()
                } else {
                    fields[5].split(',').map(str::to_owned).collect()
                },
            }),
            divergent: false,
        })
    }

    pub fn diff_for_round(&self, repo: &Path, change_id: &str) -> Result<String> {
        require_change_id(change_id)?;
        self.run_read(repo, &["diff", "-r", change_id, "--git"])
    }

    pub fn diff_cumulative(&self, repo: &Path, first: &str, last: &str) -> Result<String> {
        require_change_id(first)?;
        require_change_id(last)?;
        self.run_read(
            repo,
            &[
                "diff",
                "--from",
                &format!("{first}-"),
                "--to",
                last,
                "--git",
            ],
        )
    }

    pub fn fetch(&self, repo: &Path, remote: &str) -> Result<()> {
        self.run_mutation(repo, &["git", "fetch", "--remote", remote])
            .map(drop)
    }

    pub fn rebase_onto(&self, repo: &Path, root: &str, destination: &str) -> Result<()> {
        require_change_id(root)?;
        self.run_mutation(repo, &["rebase", "-s", root, "-d", destination])
            .map(drop)
    }

    pub fn conflicted_in(&self, repo: &Path, first: &str, last: &str) -> Result<Vec<String>> {
        require_change_id(first)?;
        require_change_id(last)?;
        let revision = format!("({first}::{last}) & conflicts()");
        let stdout = self.run_read(
            repo,
            &[
                "log",
                "--no-graph",
                "-r",
                &revision,
                "-T",
                "change_id ++ \"\\n\"",
            ],
        )?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub fn set_bookmark(&self, repo: &Path, name: &str, change_id: &str) -> Result<()> {
        require_change_id(change_id)?;
        self.run_mutation(
            repo,
            &[
                "bookmark",
                "set",
                name,
                "-r",
                change_id,
                "--allow-backwards",
            ],
        )
        .map(drop)
    }

    pub fn push(&self, repo: &Path, remote: &str, bookmark: &str) -> Result<()> {
        self.run_mutation(
            repo,
            &["git", "push", "--remote", remote, "--bookmark", bookmark],
        )
        .map(drop)
    }

    pub fn change_id(&self, repo: &Path, revision: &str) -> Result<String> {
        let output = self.run_read(
            repo,
            &["log", "--no-graph", "-r", revision, "-T", "change_id"],
        )?;
        let id = output.trim();
        require_change_id(id)?;
        Ok(id.to_owned())
    }

    fn run_read(&self, repo: &Path, args: &[&str]) -> Result<String> {
        self.run(repo, args, true)
    }

    fn run_mutation(&self, repo: &Path, args: &[&str]) -> Result<String> {
        self.run(repo, args, false)
    }

    fn run(&self, repo: &Path, args: &[&str], ignore_working_copy: bool) -> Result<String> {
        let mut command = Command::new(&self.binary);
        if ignore_working_copy {
            command.arg("--ignore-working-copy");
        }
        let output = command
            .args(["--color", "never"])
            .args(args)
            .current_dir(repo)
            .output()
            .map_err(|error| {
                Error::Jj(format!(
                    "running {} in {} failed: {error}",
                    self.binary.display(),
                    repo.display()
                ))
            })?;
        if !output.status.success() {
            return Err(Error::Jj(format!(
                "jj {} failed in {}: {}",
                args.join(" "),
                repo.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|error| Error::Jj(format!("jj returned non-UTF-8 output: {error}")))
    }
}

pub fn is_full_change_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| (b'k'..=b'z').contains(&byte))
}

fn require_change_id(value: &str) -> Result<()> {
    if !is_full_change_id(value) {
        return Err(Error::Invalid(format!("not a full jj change id: {value}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_full_change_ids_pass() {
        assert!(is_full_change_id(&"k".repeat(32)));
        assert!(!is_full_change_id(&"k".repeat(31)));
        assert!(!is_full_change_id(&"a".repeat(32)));
        assert!(!is_full_change_id("all()"));
    }
}
