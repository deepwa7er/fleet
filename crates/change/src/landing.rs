//! Landing and its durable, best-effort tail.
//!
//! The push is the irreversible boundary. Failures before it return the
//! change to review; failures after it are recorded on the shipped change and
//! can be retried explicitly with `dw finish`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    Change, ChangeService, ChangeState, DeployTrigger, Error, Record, RecordConfig, Result,
    TugboatClient, TugboatConfig,
};

const DEFAULT_PUSH_ATTEMPTS: usize = 3;

enum CoreOutcome {
    Landed,
    ReturnedToReview,
}

struct CoreFailure {
    error: Error,
    pushed: bool,
}

impl CoreFailure {
    fn before_push(error: Error) -> Self {
        Self {
            error,
            pushed: false,
        }
    }

    fn after_push(error: Error) -> Self {
        Self {
            error,
            pushed: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FizzyConfig {
    pub base: String,
    pub account: String,
    pub token_file: PathBuf,
    pub timeout: Duration,
}

impl FizzyConfig {
    pub fn from_env() -> Option<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        Some(Self {
            base: std::env::var("FIZZY_BASE")
                .unwrap_or_else(|_| "https://fizzy.intern.deepwa7er.net".to_owned()),
            account: std::env::var("FIZZY_ACCOUNT").unwrap_or_else(|_| "1".to_owned()),
            token_file: std::env::var_os("FIZZY_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config/fizzy/write-token")),
            timeout: Duration::from_secs(15),
        })
    }
}

#[derive(Debug, Clone)]
pub struct LandingConfig {
    pub remote: String,
    pub bookmark: String,
    pub push_attempts: usize,
    pub record: Option<RecordConfig>,
    pub tugboat: Option<TugboatConfig>,
    pub fizzy: Option<FizzyConfig>,
}

impl LandingConfig {
    pub fn from_env() -> Self {
        Self {
            remote: std::env::var("SKIFF_LANDING_REMOTE").unwrap_or_else(|_| "origin".to_owned()),
            bookmark: std::env::var("SKIFF_LANDING_BOOKMARK").unwrap_or_else(|_| "main".to_owned()),
            push_attempts: DEFAULT_PUSH_ATTEMPTS,
            record: RecordConfig::from_env(),
            tugboat: TugboatConfig::from_env(),
            fizzy: FizzyConfig::from_env(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailReport {
    pub record_attempted: bool,
    pub card_comment_attempted: bool,
    pub deploy_triggered: bool,
    pub deploy_jobs_finished: usize,
}

#[derive(Clone)]
pub struct LandingService {
    changes: ChangeService,
    config: LandingConfig,
    tugboat: Option<Arc<dyn DeployTrigger>>,
    record: Option<Record>,
}

impl LandingService {
    pub fn new(changes: ChangeService, config: LandingConfig) -> Result<Self> {
        if config.remote.trim().is_empty() || config.bookmark.trim().is_empty() {
            return Err(Error::Invalid(
                "landing remote and bookmark must not be empty".to_owned(),
            ));
        }
        if config.push_attempts == 0 {
            return Err(Error::Invalid(
                "landing push attempts must be positive".to_owned(),
            ));
        }
        let tugboat = config
            .tugboat
            .clone()
            .map(TugboatClient::new)
            .transpose()?
            .map(|client| Arc::new(client) as Arc<dyn DeployTrigger>);
        let record = config.record.clone().map(Record::new).transpose()?;
        Ok(Self {
            changes,
            config,
            tugboat,
            record,
        })
    }

    pub fn changes(&self) -> &ChangeService {
        &self.changes
    }

    pub fn tugboat(&self) -> Option<&dyn DeployTrigger> {
        self.tugboat.as_deref()
    }

    /// Atomically claim an in-review change for the asynchronous lander.
    pub fn begin(&self, repo: &str, card: u64) -> Result<Change> {
        self.changes.transition(repo, card, ChangeState::Landing)
    }

    /// Run the hard landing boundary, then every configured tail consequence.
    pub async fn land(&self, repo: &str, card: u64) -> Result<TailReport> {
        let core = self.clone();
        let repo_owned = repo.to_owned();
        let core_result = tokio::task::spawn_blocking(move || core.land_core(&repo_owned, card))
            .await
            .map_err(|error| Error::External(format!("landing task panicked: {error}")))?;
        match core_result {
            Ok(CoreOutcome::Landed) => self.finish(repo, card).await,
            Ok(CoreOutcome::ReturnedToReview) => Ok(TailReport::default()),
            Err(failure) => {
                if !failure.pushed {
                    let reason = format!("landing failed: {}", failure.error);
                    let _ = self.changes.store().fail_landing(repo, card, &reason, &[]);
                }
                Err(failure.error)
            }
        }
    }

    fn land_core(&self, repo: &str, card: u64) -> std::result::Result<CoreOutcome, CoreFailure> {
        let path = self
            .changes
            .repository(repo)
            .map_err(CoreFailure::before_push)?;
        let change = self
            .changes
            .store()
            .require(repo, card)
            .map_err(CoreFailure::before_push)?;
        if change.state != ChangeState::Landing {
            return Err(CoreFailure::before_push(Error::Transition(format!(
                "change {repo}/{card} is {}, not landing",
                change.state
            ))));
        }
        let first = change
            .rounds
            .first()
            .ok_or_else(|| Error::Invalid(format!("change {repo}/{card} has no rounds")))
            .map_err(CoreFailure::before_push)?
            .change_id
            .clone();
        let last = change
            .rounds
            .last()
            .expect("first round exists")
            .change_id
            .clone();
        let destination = format!("{}@{}", self.config.bookmark, self.config.remote);
        let mut last_push = None;
        for _ in 0..self.config.push_attempts {
            self.changes
                .jj()
                .fetch(&path, &self.config.remote)
                .map_err(CoreFailure::before_push)?;
            self.changes
                .jj()
                .rebase_onto(&path, &first, &destination)
                .map_err(CoreFailure::before_push)?;
            let conflicts = self
                .changes
                .jj()
                .conflicted_in(&path, &first, &last)
                .map_err(CoreFailure::before_push)?;
            if !conflicts.is_empty() {
                self.changes
                    .store()
                    .fail_landing(
                        repo,
                        card,
                        "the rebase onto main conflicts; resolve it as the next round",
                        &conflicts,
                    )
                    .map_err(CoreFailure::before_push)?;
                return Ok(CoreOutcome::ReturnedToReview);
            }
            let tip = self
                .changes
                .jj()
                .show(&path, &last)
                .map_err(CoreFailure::before_push)?
                .commit
                .ok_or_else(|| {
                    CoreFailure::before_push(Error::Jj(format!(
                        "landing change {last} no longer resolves in {repo}"
                    )))
                })?;
            self.changes
                .jj()
                .set_bookmark(&path, &self.config.bookmark, &last)
                .map_err(CoreFailure::before_push)?;
            match self
                .changes
                .jj()
                .push(&path, &self.config.remote, &self.config.bookmark)
            {
                Ok(()) => {
                    self.changes
                        .store()
                        .complete_landing(repo, card, &tip.commit_id)
                        .map_err(CoreFailure::after_push)?;
                    return Ok(CoreOutcome::Landed);
                }
                Err(error) => last_push = Some(error.to_string()),
            }
        }
        self.changes
            .store()
            .fail_landing(
                repo,
                card,
                &format!(
                    "push lost the race {} times: {}",
                    self.config.push_attempts,
                    last_push.as_deref().unwrap_or("unknown push failure")
                ),
                &[],
            )
            .map_err(CoreFailure::before_push)?;
        Ok(CoreOutcome::ReturnedToReview)
    }

    /// Retry only unfinished or failed post-push consequences.
    pub async fn finish(&self, repo: &str, card: u64) -> Result<TailReport> {
        let change = self.changes.store().require(repo, card)?;
        if change.state != ChangeState::Shipped {
            return Err(Error::Transition(format!(
                "change {repo}/{card} is {}; only shipped changes have a landing tail to finish",
                change.state
            )));
        }
        let mut report = TailReport::default();

        // Trigger first, matching the transaction's documented order. Polling
        // waits until the other metadata has had its chance, so a ten-minute
        // deploy never delays the public record or card comment.
        let deploy_services = if let Some(tugboat) = &self.tugboat {
            let latest = self.changes.store().require(repo, card)?;
            Some(
                if latest
                    .deploy
                    .as_ref()
                    .is_some_and(|deploy| deploy.error.is_none())
                {
                    latest
                        .deploy
                        .map(|deploy| deploy.services)
                        .unwrap_or_default()
                } else {
                    report.deploy_triggered = true;
                    match tugboat.trigger_all().await {
                        Ok(services) => {
                            let _ = self
                                .changes
                                .store()
                                .record_deploy(repo, card, &services, None);
                            services
                        }
                        Err(error) => {
                            let _ = self.changes.store().record_deploy(
                                repo,
                                card,
                                &[],
                                Some(&error.to_string()),
                            );
                            Vec::new()
                        }
                    }
                },
            )
        } else {
            None
        };

        if self.record.is_some()
            && !change
                .record_export
                .as_ref()
                .is_some_and(|outcome| outcome.ok)
        {
            report.record_attempted = true;
            self.finish_record(repo, card).await;
        }
        if self.config.fizzy.is_some()
            && !change
                .card_comment
                .as_ref()
                .is_some_and(|outcome| outcome.ok)
        {
            report.card_comment_attempted = true;
            self.finish_card_comment(repo, card).await;
        }
        if let Some(services) = deploy_services {
            report.deploy_jobs_finished = self.poll_deploy(repo, card, &services).await;
        }
        Ok(report)
    }

    async fn finish_record(&self, repo: &str, card: u64) {
        let Some(record) = self.record.clone() else {
            return;
        };
        let changes = self.changes.clone();
        let repo_owned = repo.to_owned();
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let change = changes.get(&repo_owned, card)?;
            let mut diffs = BTreeMap::new();
            for round in &change.rounds {
                diffs.insert(
                    round.n,
                    changes.round_diff_text(&repo_owned, card, round.n)?,
                );
            }
            record.export(&change, &diffs)?;
            Ok(())
        })
        .await;
        let outcome = match result {
            Ok(Ok(())) => (true, None),
            Ok(Err(error)) => (false, Some(error.to_string())),
            Err(error) => (false, Some(format!("record task panicked: {error}"))),
        };
        let _ = self
            .changes
            .store()
            .record_export(repo, card, outcome.0, outcome.1.as_deref());
    }

    async fn finish_card_comment(&self, repo: &str, card: u64) {
        let Some(config) = &self.config.fizzy else {
            return;
        };
        let result = async {
            let token = tokio::fs::read_to_string(&config.token_file)
                .await
                .map_err(|error| {
                    Error::External(format!(
                        "reading Fizzy token {}: {error}",
                        config.token_file.display()
                    ))
                })?;
            let token = token.trim();
            if token.is_empty() {
                return Err(Error::External(format!(
                    "Fizzy token file is empty: {}",
                    config.token_file.display()
                )));
            }
            let change = self.changes.store().require(repo, card)?;
            let client = fizzy::Client::new(
                &config.base,
                &config.account,
                token.to_owned(),
                config.timeout,
            )
            .map_err(|error| Error::External(format!("building Fizzy client: {error:#}")))?;
            let card_number = i64::try_from(card)
                .map_err(|_| Error::Invalid(format!("card number does not fit Fizzy: {card}")))?;
            client
                .comment_on_card(card_number, &landed_comment(&change)?)
                .await
                .map_err(|error| Error::External(format!("Fizzy comment failed: {error:#}")))?;
            Ok::<(), Error>(())
        }
        .await;
        let (ok, message) = match result {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        };
        let _ = self
            .changes
            .store()
            .record_card_comment(repo, card, ok, message.as_deref());
    }

    async fn poll_deploy(&self, repo: &str, card: u64, services: &[crate::DeployService]) -> usize {
        let Some(tugboat) = &self.tugboat else {
            return 0;
        };
        let mut pending: Vec<String> = services
            .iter()
            .filter(|service| service.outcome.is_none())
            .filter_map(|service| service.job_id.clone())
            .collect();
        let deadline = Instant::now() + tugboat.poll_deadline();
        let mut finished = 0;
        while !pending.is_empty() && Instant::now() < deadline {
            for job_id in pending.clone() {
                if let Ok(Some(outcome)) = tugboat.job_outcome(&job_id).await {
                    let _ = self.changes.store().record_deploy_outcome(
                        repo,
                        card,
                        &job_id,
                        outcome.ok,
                        outcome.message.as_deref(),
                    );
                    pending.retain(|pending| pending != &job_id);
                    finished += 1;
                }
            }
            if !pending.is_empty() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::time::sleep(tugboat.poll_interval().min(remaining)).await;
            }
        }
        finished
    }
}

fn landed_comment(change: &Change) -> Result<String> {
    let landed = change.landed.as_ref().ok_or_else(|| {
        Error::Invalid(format!(
            "change {}/{} is not landed",
            change.repo, change.card
        ))
    })?;
    let rounds = change.rounds.len();
    let title = change
        .title
        .as_deref()
        .map(|title| format!("{} — ", escape_html(title)))
        .unwrap_or_default();
    Ok(format!(
        "<p>Landed: {title}{rounds} round{} of {} change #{}, tip {}.</p>",
        if rounds == 1 { "" } else { "s" },
        escape_html(&change.repo),
        change.card,
        escape_html(&landed.tip.chars().take(12).collect::<String>())
    ))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Landed, Store};

    #[test]
    fn card_comment_escapes_authored_text() {
        let store = tempfile::tempdir().unwrap();
        let store = Store::new(store.path());
        let mut change = store
            .create("fleet", 4, Some("<model & picker>"), None)
            .unwrap();
        change.landed = Some(Landed {
            tip: "abcdef123456789".to_owned(),
            at: "now".to_owned(),
        });
        assert_eq!(
            landed_comment(&change).unwrap(),
            "<p>Landed: &lt;model &amp; picker&gt; — 0 rounds of fleet change #4, tip abcdef123456.</p>"
        );
    }
}
