//! The append-only authored log.
//!
//! `dw` and Skiff are separate processes, so each change has an advisory
//! lock file. Every read/validate/append mutation holds that lock across the
//! whole operation; relying on an in-process mutex here would lose rounds.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::is_full_change_id;
use crate::model::{
    Annotation, AnnotationSide, Author, CardComment, Change, ChangeState, Deploy, DeployOutcome,
    DeployService, Landed, Landing, RecordExport, Request, Round,
};
use crate::{Error, Result, io, validate_card, validate_repo};

#[derive(Debug, Clone)]
pub struct RoundInput {
    pub author: Author,
    pub change_id: String,
    pub note: Option<String>,
    pub gates_ran: Vec<String>,
    pub worth_knowing: Vec<String>,
}

impl RoundInput {
    pub fn validate(&self) -> Result<()> {
        if !is_full_change_id(&self.change_id) {
            return Err(Error::Invalid(format!(
                "round requires a full jj change id, got {}",
                self.change_id
            )));
        }
        validate_optional_text("round note", self.note.as_deref())?;
        validate_claims("gatesRan", &self.gates_ran)?;
        validate_claims("worthKnowing", &self.worth_knowing)
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create(
        &self,
        repo: &str,
        card: u64,
        title: Option<&str>,
        session: Option<&str>,
    ) -> Result<Change> {
        validate_key(repo, card)?;
        validate_optional_nonempty("title", title)?;
        validate_optional_nonempty("session", session)?;
        self.exclusive(repo, card, || {
            let path = self.log_path(repo, card);
            let at = now();
            let event = json!({
                "event": "created",
                "repo": repo,
                "card": card,
                "title": title,
                "session": session,
                "at": at,
            });
            let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return match replay_file(&path)? {
                        Some(_) => Err(Error::Exists {
                            repo: repo.to_owned(),
                            card,
                        }),
                        None => Err(Error::Invalid(format!(
                            "{} exists but contains no created event; inspect it before retrying",
                            path.display()
                        ))),
                    };
                }
                Err(error) => return Err(io(format!("creating {}", path.display()))(error)),
            };
            write_events(&mut file, &[event], &path)?;
            sync_parent(&path)?;
            replay_file(&path)?
                .ok_or_else(|| Error::Invalid("created log did not replay".to_owned()))
        })
    }

    pub fn get(&self, repo: &str, card: u64) -> Result<Option<Change>> {
        validate_key(repo, card)?;
        self.shared(repo, card, || replay_file(&self.log_path(repo, card)))
    }

    pub fn require(&self, repo: &str, card: u64) -> Result<Change> {
        self.get(repo, card)?.ok_or_else(|| Error::NotFound {
            repo: repo.to_owned(),
            card,
        })
    }

    pub fn list(&self) -> Result<Vec<Change>> {
        let repos = match fs::read_dir(&self.root) {
            Ok(repos) => repos,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io(format!("reading {}", self.root.display()))(error)),
        };
        let mut changes = Vec::new();
        for repo in repos {
            let repo = repo.map_err(io(format!("reading {}", self.root.display())))?;
            let file_type = repo
                .file_type()
                .map_err(io(format!("reading {}", repo.path().display())))?;
            if !file_type.is_dir() {
                continue;
            }
            let repo_name = repo.file_name().to_string_lossy().into_owned();
            if validate_repo(&repo_name).is_err() {
                continue;
            }
            for file in fs::read_dir(repo.path())
                .map_err(io(format!("reading {}", repo.path().display())))?
            {
                let file = file.map_err(io(format!("reading {}", repo.path().display())))?;
                let Some(stem) = file
                    .path()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                if file.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(card) = stem.parse::<u64>() else {
                    continue;
                };
                if let Some(change) = self.get(&repo_name, card)? {
                    changes.push(change);
                }
            }
        }
        changes.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(changes)
    }

    pub fn bound_to(&self, session: &str) -> Result<Option<crate::ChangeRef>> {
        if session.is_empty() {
            return Err(Error::Invalid("session must be non-empty".to_owned()));
        }
        Ok(self
            .list()?
            .into_iter()
            .find(|change| change.session.as_deref() == Some(session))
            .map(|change| change.reference()))
    }

    pub fn add_round(
        &self,
        repo: &str,
        card: u64,
        input: RoundInput,
        validate: impl FnOnce(&Change) -> Result<()>,
    ) -> Result<Round> {
        validate_key(repo, card)?;
        input.validate()?;
        self.mutate(repo, card, |change| {
            ensure_appendable(change, "rounds")?;
            if let Some(existing) = change
                .rounds
                .iter()
                .find(|round| round.change_id == input.change_id)
            {
                return Err(Error::DuplicateRound {
                    change_id: input.change_id.clone(),
                    round: existing.n,
                });
            }
            validate(change)?;
            let at = now();
            let n = change.rounds.len() as u32 + 1;
            let event = json!({
                "event": "round",
                "n": n,
                "author": input.author,
                "changeId": input.change_id,
                "note": input.note,
                "gatesRan": input.gates_ran,
                "worthKnowing": input.worth_knowing,
                "at": at,
            });
            Ok((vec![event], n))
        })
        .and_then(|(change, n)| {
            change
                .rounds
                .into_iter()
                .find(|round| round.n == n)
                .ok_or_else(|| Error::Invalid(format!("round {n} did not replay")))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_annotation(
        &self,
        repo: &str,
        card: u64,
        round: u32,
        path: &str,
        line: u32,
        side: AnnotationSide,
        text: &str,
        validate: impl FnOnce(&Change, &Round) -> Result<()>,
    ) -> Result<Annotation> {
        validate_key(repo, card)?;
        if round == 0 {
            return Err(Error::Invalid(
                "annotation requires a round number".to_owned(),
            ));
        }
        if path.is_empty() || line == 0 || text.trim().is_empty() {
            return Err(Error::Invalid(
                "annotation requires a path, positive line, and non-empty text".to_owned(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.mutate(repo, card, |change| {
            ensure_appendable(change, "annotations")?;
            let target = change
                .rounds
                .iter()
                .find(|candidate| candidate.n == round)
                .ok_or_else(|| Error::NoRound {
                    repo: repo.to_owned(),
                    card,
                    round,
                })?;
            validate(change, target)?;
            let event = json!({
                "event": "annotation",
                "id": id,
                "round": round,
                "path": path,
                "line": line,
                "side": side,
                "text": text,
                "at": now(),
            });
            Ok((vec![event], id.clone()))
        })
        .and_then(|(change, id)| {
            change
                .rounds
                .into_iter()
                .flat_map(|round| round.annotations)
                .find(|annotation| annotation.id == id)
                .ok_or_else(|| Error::Invalid(format!("annotation {id} did not replay")))
        })
    }

    pub fn set_session(&self, repo: &str, card: u64, session: &str) -> Result<Change> {
        validate_key(repo, card)?;
        if session.is_empty() {
            return Err(Error::Invalid("session must be non-empty".to_owned()));
        }
        self.mutate(repo, card, |_| {
            Ok((
                vec![json!({ "event": "session", "session": session, "at": now() })],
                (),
            ))
        })
        .map(|(change, ())| change)
    }

    pub fn transition(&self, repo: &str, card: u64, state: ChangeState) -> Result<Change> {
        validate_key(repo, card)?;
        self.mutate(repo, card, |change| {
            if !change.state.can_transition_to(state) {
                return Err(Error::Transition(format!(
                    "change {repo}/{card} is {}; cannot move to {state}",
                    change.state
                )));
            }
            if state == ChangeState::InReview && change.rounds.is_empty() {
                return Err(Error::Transition(format!(
                    "change {repo}/{card} has no rounds; nothing to review"
                )));
            }
            Ok((
                vec![json!({ "event": "state", "state": state, "at": now() })],
                (),
            ))
        })
        .map(|(change, ())| change)
    }

    pub fn request_changes(&self, repo: &str, card: u64, note: &str) -> Result<Change> {
        validate_key(repo, card)?;
        if note.trim().is_empty() {
            return Err(Error::Invalid("request requires a note".to_owned()));
        }
        self.mutate(repo, card, |change| {
            if change.state != ChangeState::InReview {
                return Err(Error::Transition(format!(
                    "change {repo}/{card} is {}; only in_review changes take requests",
                    change.state
                )));
            }
            // One event owns both facts, so a crash cannot record the note
            // without reopening the change (or vice versa).
            Ok((
                vec![json!({ "event": "requested", "note": note, "at": now() })],
                (),
            ))
        })
        .map(|(change, ())| change)
    }

    pub fn complete_landing(&self, repo: &str, card: u64, tip: &str) -> Result<Change> {
        validate_key(repo, card)?;
        if tip.is_empty() {
            return Err(Error::Invalid(
                "complete landing requires the tip commit".to_owned(),
            ));
        }
        self.landing_outcome(
            repo,
            card,
            json!({ "event": "landed", "tip": tip, "at": now() }),
        )
    }

    pub fn fail_landing(
        &self,
        repo: &str,
        card: u64,
        reason: &str,
        conflicts: &[String],
    ) -> Result<Change> {
        validate_key(repo, card)?;
        if reason.is_empty() {
            return Err(Error::Invalid(
                "failed landing requires a reason".to_owned(),
            ));
        }
        self.landing_outcome(
            repo,
            card,
            json!({ "event": "landing_failed", "reason": reason, "conflicts": conflicts, "at": now() }),
        )
    }

    fn landing_outcome(&self, repo: &str, card: u64, event: Value) -> Result<Change> {
        self.mutate(repo, card, |change| {
            if change.state != ChangeState::Landing {
                return Err(Error::Transition(format!(
                    "change {repo}/{card} is {}, not landing",
                    change.state
                )));
            }
            Ok((vec![event], ()))
        })
        .map(|(change, ())| change)
    }

    pub fn record_card_comment(
        &self,
        repo: &str,
        card: u64,
        ok: bool,
        message: Option<&str>,
    ) -> Result<Change> {
        self.outcome(repo, card, "card_comment", ok, message)
    }

    pub fn record_export(
        &self,
        repo: &str,
        card: u64,
        ok: bool,
        message: Option<&str>,
    ) -> Result<Change> {
        self.outcome(repo, card, "recorded", ok, message)
    }

    fn outcome(
        &self,
        repo: &str,
        card: u64,
        event: &str,
        ok: bool,
        message: Option<&str>,
    ) -> Result<Change> {
        validate_key(repo, card)?;
        self.mutate(repo, card, |_| {
            Ok((
                vec![json!({ "event": event, "ok": ok, "message": message, "at": now() })],
                (),
            ))
        })
        .map(|(change, ())| change)
    }

    pub fn record_deploy(
        &self,
        repo: &str,
        card: u64,
        services: &[DeployService],
        error: Option<&str>,
    ) -> Result<Change> {
        validate_key(repo, card)?;
        self.mutate(repo, card, |_| {
            Ok((vec![json!({ "event": "deploy", "triggered": services, "error": error, "at": now() })], ()))
        })
        .map(|(change, ())| change)
    }

    pub fn record_deploy_outcome(
        &self,
        repo: &str,
        card: u64,
        job_id: &str,
        ok: bool,
        message: Option<&str>,
    ) -> Result<Change> {
        validate_key(repo, card)?;
        if job_id.is_empty() {
            return Err(Error::Invalid(
                "deploy outcome requires a job id".to_owned(),
            ));
        }
        self.mutate(repo, card, |_| {
            Ok((
                vec![json!({
                    "event": "deploy_outcome",
                    "jobId": job_id,
                    "ok": ok,
                    "message": message,
                    "at": now(),
                })],
                (),
            ))
        })
        .map(|(change, ())| change)
    }

    fn mutate<T>(
        &self,
        repo: &str,
        card: u64,
        operation: impl FnOnce(&Change) -> Result<(Vec<Value>, T)>,
    ) -> Result<(Change, T)> {
        self.exclusive(repo, card, || {
            let path = self.log_path(repo, card);
            let change = replay_file(&path)?.ok_or_else(|| Error::NotFound {
                repo: repo.to_owned(),
                card,
            })?;
            let (events, answer) = operation(&change)?;
            let mut file = OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(io(format!("opening {}", path.display())))?;
            write_events(&mut file, &events, &path)?;
            let changed = replay_file(&path)?.ok_or_else(|| {
                Error::Invalid(format!("{} disappeared after append", path.display()))
            })?;
            Ok((changed, answer))
        })
    }

    fn log_path(&self, repo: &str, card: u64) -> PathBuf {
        self.root.join(repo).join(format!("{card}.jsonl"))
    }

    fn lock_path(&self, repo: &str, card: u64) -> PathBuf {
        self.root.join(repo).join(format!("{card}.lock"))
    }

    fn shared<T>(&self, repo: &str, card: u64, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.locked(repo, card, false, operation)
    }

    fn exclusive<T>(
        &self,
        repo: &str,
        card: u64,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.locked(repo, card, true, operation)
    }

    fn locked<T>(
        &self,
        repo: &str,
        card: u64,
        exclusive: bool,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let lock_path = self.lock_path(repo, card);
        let parent = lock_path.parent().expect("a lock always has a parent");
        fs::create_dir_all(parent).map_err(io(format!("creating {}", parent.display())))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(io(format!("opening {}", lock_path.display())))?;
        if exclusive {
            lock.lock()
        } else {
            lock.lock_shared()
        }
        .map_err(io(format!("locking {}", lock_path.display())))?;
        let answer = operation();
        lock.unlock()
            .map_err(io(format!("unlocking {}", lock_path.display())))?;
        answer
    }
}

fn validate_key(repo: &str, card: u64) -> Result<()> {
    validate_repo(repo)?;
    validate_card(card)
}

fn validate_optional_nonempty(name: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(Error::Invalid(format!("{name} must be a non-empty string")));
    }
    Ok(())
}

fn validate_optional_text(name: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| value.contains('\0')) {
        return Err(Error::Invalid(format!("{name} cannot contain NUL")));
    }
    Ok(())
}

fn validate_claims(name: &str, claims: &[String]) -> Result<()> {
    if claims.iter().any(|claim| claim.trim().is_empty()) {
        return Err(Error::Invalid(format!(
            "{name} must contain only non-empty strings"
        )));
    }
    Ok(())
}

fn ensure_appendable(change: &Change, operation: &'static str) -> Result<()> {
    if !change.state.is_appendable() {
        return Err(Error::Frozen {
            repo: change.repo.clone(),
            card: change.card,
            state: change.state,
            operation,
        });
    }
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn write_events(file: &mut File, events: &[Value], path: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).map_err(|source| Error::Json {
            context: format!("serializing an event for {}", path.display()),
            source,
        })?;
        bytes.push(b'\n');
    }
    file.write_all(&bytes)
        .map_err(io(format!("writing {}", path.display())))?;
    file.sync_all()
        .map_err(io(format!("syncing {}", path.display())))
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().expect("an event log always has a parent");
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(io(format!("syncing directory {}", parent.display())))
}

fn replay_file(path: &Path) -> Result<Option<Change>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(format!("opening {}", path.display()))(error)),
    };
    let mut change = None;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(io(format!("reading {}", path.display())))?;
        let Ok(event) = serde_json::from_str::<Value>(line.trim_end_matches('\r')) else {
            continue;
        };
        apply_event(&mut change, &event);
    }
    Ok(change)
}

fn apply_event(change: &mut Option<Change>, event: &Value) {
    let Some(kind) = event.get("event").and_then(Value::as_str) else {
        return;
    };
    if kind == "created" {
        let (Some(repo), Some(card), Some(at)) = (
            event.get("repo").and_then(Value::as_str),
            event.get("card").and_then(Value::as_u64),
            event.get("at").and_then(Value::as_str),
        ) else {
            return;
        };
        *change = Some(Change {
            repo: repo.to_owned(),
            card,
            title: optional_string(event, "title"),
            session: optional_string(event, "session"),
            state: ChangeState::Working,
            created_at: at.to_owned(),
            updated_at: at.to_owned(),
            rounds: Vec::new(),
            last_request: None,
            landed: None,
            last_landing: None,
            card_comment: None,
            record_export: None,
            deploy: None,
            path: None,
        });
        return;
    }
    let Some(change) = change else { return };
    let Some(at) = event.get("at").and_then(Value::as_str) else {
        return;
    };
    let recognized = match kind {
        "round" => replay_round(change, event, at),
        "annotation" => replay_annotation(change, event, at),
        "session" => event
            .get("session")
            .and_then(Value::as_str)
            .map(|session| change.session = Some(session.to_owned()))
            .is_some(),
        "requested" => event
            .get("note")
            .and_then(Value::as_str)
            .map(|note| {
                change.last_request = Some(Request {
                    note: note.to_owned(),
                    at: at.to_owned(),
                });
                change.state = ChangeState::Working;
            })
            .is_some(),
        "landed" => event
            .get("tip")
            .and_then(Value::as_str)
            .map(|tip| {
                change.landed = Some(Landed {
                    tip: tip.to_owned(),
                    at: at.to_owned(),
                });
                change.last_landing = Some(Landing {
                    ok: true,
                    reason: None,
                    conflicts: Vec::new(),
                    at: at.to_owned(),
                });
                change.state = ChangeState::Shipped;
            })
            .is_some(),
        "landing_failed" => event
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| {
                change.last_landing = Some(Landing {
                    ok: false,
                    reason: Some(reason.to_owned()),
                    conflicts: strings(event.get("conflicts")),
                    at: at.to_owned(),
                });
                change.state = ChangeState::InReview;
            })
            .is_some(),
        "state" => {
            serde_json::from_value::<ChangeState>(event.get("state").cloned().unwrap_or_default())
                .map(|state| change.state = state)
                .is_ok()
        }
        "card_comment" => replay_outcome(event, at)
            .map(|outcome| change.card_comment = Some(outcome))
            .is_some(),
        "recorded" => replay_record(event, at)
            .map(|outcome| change.record_export = Some(outcome))
            .is_some(),
        "deploy" => replay_deploy(change, event, at),
        "deploy_outcome" => replay_deploy_outcome(change, event),
        _ => false,
    };
    if recognized {
        change.updated_at = at.to_owned();
    }
}

fn replay_round(change: &mut Change, event: &Value, at: &str) -> bool {
    let (Some(n), Some(author), Some(change_id)) = (
        event
            .get("n")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        event
            .get("author")
            .cloned()
            .and_then(|author| serde_json::from_value(author).ok()),
        event.get("changeId").and_then(Value::as_str),
    ) else {
        return false;
    };
    change.rounds.push(Round {
        n,
        author,
        change_id: change_id.to_owned(),
        note: optional_string(event, "note"),
        gates_ran: strings(event.get("gatesRan")),
        worth_knowing: strings(event.get("worthKnowing")),
        created_at: at.to_owned(),
        annotations: Vec::new(),
        commit: None,
        divergent: false,
    });
    true
}

fn replay_annotation(change: &mut Change, event: &Value, at: &str) -> bool {
    let (Some(id), Some(round), Some(path), Some(line), Some(side), Some(text)) = (
        event.get("id").and_then(Value::as_str),
        event
            .get("round")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        event.get("path").and_then(Value::as_str),
        event
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        event
            .get("side")
            .cloned()
            .and_then(|side| serde_json::from_value(side).ok()),
        event.get("text").and_then(Value::as_str),
    ) else {
        return false;
    };
    let Some(target) = change
        .rounds
        .iter_mut()
        .find(|candidate| candidate.n == round)
    else {
        return false;
    };
    target.annotations.push(Annotation {
        id: id.to_owned(),
        path: path.to_owned(),
        line,
        side,
        text: text.to_owned(),
        created_at: at.to_owned(),
    });
    true
}

fn replay_outcome(event: &Value, at: &str) -> Option<CardComment> {
    Some(CardComment {
        ok: event.get("ok")?.as_bool()?,
        message: optional_string(event, "message"),
        at: at.to_owned(),
    })
}

fn replay_record(event: &Value, at: &str) -> Option<RecordExport> {
    Some(RecordExport {
        ok: event.get("ok")?.as_bool()?,
        message: optional_string(event, "message"),
        at: at.to_owned(),
    })
}

fn replay_deploy(change: &mut Change, event: &Value, at: &str) -> bool {
    let services = event
        .get("triggered")
        .cloned()
        .and_then(|services| serde_json::from_value(services).ok())
        .unwrap_or_default();
    change.deploy = Some(Deploy {
        at: at.to_owned(),
        error: optional_string(event, "error"),
        services,
    });
    true
}

fn replay_deploy_outcome(change: &mut Change, event: &Value) -> bool {
    let (Some(job_id), Some(ok)) = (
        event.get("jobId").and_then(Value::as_str),
        event.get("ok").and_then(Value::as_bool),
    ) else {
        return false;
    };
    let Some(service) = change.deploy.as_mut().and_then(|deploy| {
        deploy
            .services
            .iter_mut()
            .find(|service| service.job_id.as_deref() == Some(job_id))
    }) else {
        return false;
    };
    service.outcome = Some(DeployOutcome {
        ok,
        message: optional_string(event, "message"),
    });
    true
}

fn optional_string(event: &Value, key: &str) -> Option<String> {
    event.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
