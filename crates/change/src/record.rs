//! DW-003 public-record export.
//!
//! `build_public_change` is the privacy boundary. It copies allowed fields
//! one by one; new private domain fields cannot leak by serialization.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{AnnotationSide, Author, Change, Error, Result, io};

#[derive(Debug, Clone)]
pub struct RecordConfig {
    pub dir: PathBuf,
    pub remote: String,
    pub git_binary: PathBuf,
}

impl RecordConfig {
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os("SKIFF_RECORD_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join("code/record"))
            })?;
        Some(Self {
            dir,
            remote: std::env::var("SKIFF_RECORD_REMOTE").unwrap_or_else(|_| "origin".to_owned()),
            git_binary: std::env::var_os("GIT_BINARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| "git".into()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicChange {
    pub repo: String,
    pub card: u64,
    pub title: Option<String>,
    pub landed_at: String,
    pub tip: String,
    pub rounds: Vec<PublicRound>,
    pub afterward: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicRound {
    pub n: u32,
    pub author: Author,
    pub change_id: String,
    pub commit: Option<String>,
    pub gates_ran: Vec<String>,
    pub worth_knowing: Vec<String>,
    pub diff: Option<String>,
    pub annotations: Vec<PublicAnnotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAnnotation {
    pub path: String,
    pub line: u32,
    pub side: AnnotationSide,
    pub text: String,
}

pub fn build_public_change(change: &Change, diffs: &BTreeMap<u32, String>) -> Result<PublicChange> {
    let landed = change.landed.as_ref().ok_or_else(|| {
        Error::Invalid(format!(
            "change {}/{} is not landed; it cannot enter the public record",
            change.repo, change.card
        ))
    })?;
    Ok(PublicChange {
        repo: change.repo.clone(),
        card: change.card,
        title: change.title.clone(),
        landed_at: landed.at.clone(),
        tip: landed.tip.clone(),
        rounds: change
            .rounds
            .iter()
            .map(|round| PublicRound {
                n: round.n,
                author: round.author,
                change_id: round.change_id.clone(),
                commit: round.commit.as_ref().map(|commit| commit.commit_id.clone()),
                gates_ran: round.gates_ran.clone(),
                worth_knowing: round.worth_knowing.clone(),
                diff: diffs.get(&round.n).cloned(),
                annotations: round
                    .annotations
                    .iter()
                    .map(|annotation| PublicAnnotation {
                        path: annotation.path.clone(),
                        line: annotation.line,
                        side: annotation.side,
                        text: annotation.text.clone(),
                    })
                    .collect(),
            })
            .collect(),
        afterward: Vec::new(),
    })
}

#[derive(Clone)]
pub struct Record {
    config: RecordConfig,
}

impl Record {
    pub fn new(config: RecordConfig) -> Result<Self> {
        if config.remote.trim().is_empty() {
            return Err(Error::Invalid("record remote must not be empty".to_owned()));
        }
        Ok(Self { config })
    }

    pub fn export(&self, change: &Change, diffs: &BTreeMap<u32, String>) -> Result<PathBuf> {
        let entry = build_public_change(change, diffs)?;
        let relative = PathBuf::from(&entry.repo).join(format!("{}.json", entry.card));
        let target = self.config.dir.join(&relative);
        let lock_path = self.config.dir.join(".skiff-record.lock");
        fs::create_dir_all(&self.config.dir)
            .map_err(io(format!("creating {}", self.config.dir.display())))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(io(format!("opening {}", lock_path.display())))?;
        lock.lock()
            .map_err(io(format!("locking {}", lock_path.display())))?;
        let result = self.export_locked(&entry, &relative, &target);
        lock.unlock()
            .map_err(io(format!("unlocking {}", lock_path.display())))?;
        result.map(|()| relative)
    }

    fn export_locked(&self, entry: &PublicChange, relative: &Path, target: &Path) -> Result<()> {
        let parent = target.parent().expect("record target always has a parent");
        fs::create_dir_all(parent).map_err(io(format!("creating {}", parent.display())))?;
        let temporary = parent.join(format!(".{}.{}.tmp", entry.card, Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(entry).map_err(|source| Error::Json {
            context: format!("serializing public record {}", target.display()),
            source,
        })?;
        let write_result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io(format!("creating {}", temporary.display())))?;
            file.write_all(&bytes)
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(io(format!("writing {}", temporary.display())))?;
            fs::rename(&temporary, target)
                .map_err(io(format!("replacing {}", target.display())))?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(io(format!("syncing {}", parent.display())))
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;

        self.git(&["add", path_text(relative)?])?;
        let staged =
            self.git_status(&["diff", "--cached", "--quiet", "--", path_text(relative)?])?;
        if staged.code() == Some(1) {
            let title = entry
                .title
                .as_deref()
                .map(|title| format!(" — {title}"))
                .unwrap_or_default();
            self.git(&[
                "commit",
                "-m",
                &format!("record: {} #{}{}", entry.repo, entry.card, title),
            ])?;
        } else if !staged.success() {
            return Err(Error::External(format!(
                "git diff --cached failed in {} with {staged}",
                self.config.dir.display()
            )));
        }
        self.git(&["push", &self.config.remote, "HEAD"])?;
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let output = self.command(args).output().map_err(|error| {
            Error::External(format!(
                "running {} in {} failed: {error}",
                self.config.git_binary.display(),
                self.config.dir.display()
            ))
        })?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::External(format!(
            "record git {} failed in {}: {}",
            args.join(" "),
            self.config.dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }

    fn git_status(&self, args: &[&str]) -> Result<ExitStatus> {
        self.command(args).status().map_err(|error| {
            Error::External(format!(
                "running record git {} in {} failed: {error}",
                args.join(" "),
                self.config.dir.display()
            ))
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.config.git_binary);
        command
            .args([
                "-c",
                "user.name=skiff",
                "-c",
                "user.email=skiff@deepwa7er.net",
            ])
            .args(args)
            .current_dir(&self.config.dir);
        command
    }
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Invalid(format!(
            "record path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Annotation, ChangeState, Commit, Landed, Round};

    fn private_change() -> Change {
        Change {
            repo: "fleet".to_owned(),
            card: 81,
            title: Some("model picker".to_owned()),
            session: Some("pi:secret".to_owned()),
            state: ChangeState::Shipped,
            created_at: "created".to_owned(),
            updated_at: "updated".to_owned(),
            rounds: vec![Round {
                n: 1,
                author: Author::Agent,
                change_id: "k".repeat(32),
                note: Some("private prompt".to_owned()),
                gates_ran: vec!["cargo test".to_owned()],
                worth_knowing: vec!["one dependency".to_owned()],
                created_at: "round-at".to_owned(),
                annotations: vec![Annotation {
                    id: "private-uuid".to_owned(),
                    path: "src/main.rs".to_owned(),
                    line: 3,
                    side: AnnotationSide::New,
                    text: "why".to_owned(),
                    created_at: "annotation-at".to_owned(),
                }],
                commit: Some(Commit {
                    change_id: "k".repeat(32),
                    commit_id: "abc".to_owned(),
                    description: "description".to_owned(),
                    author_email: "private@example.invalid".to_owned(),
                    timestamp: "commit-at".to_owned(),
                    parents: Vec::new(),
                }),
                divergent: false,
            }],
            last_request: Some(crate::Request {
                note: "private request".to_owned(),
                at: "request-at".to_owned(),
            }),
            landed: Some(Landed {
                tip: "abc".to_owned(),
                at: "landed-at".to_owned(),
            }),
            last_landing: None,
            card_comment: None,
            record_export: None,
            deploy: None,
            path: Some("/private/path".to_owned()),
        }
    }

    #[test]
    fn public_entry_is_field_by_field_and_excludes_private_state() {
        let entry = build_public_change(
            &private_change(),
            &BTreeMap::from([(1, "diff --git ...".to_owned())]),
        )
        .unwrap();
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("diff --git"));
        assert!(json.contains("cargo test"));
        for private in [
            "pi:secret",
            "private prompt",
            "private request",
            "private-uuid",
            "private@example.invalid",
            "/private/path",
        ] {
            assert!(!json.contains(private), "leaked {private}");
        }
    }
}
