//! Repository-aware composition of the log, jj, and structured diff.

use std::path::PathBuf;

use crate::{
    Annotation, AnnotationSide, Change, ChangeState, Diff, Error, Jj, Result, Round, RoundInput,
    Store, parse_diff, repository_path,
};

#[derive(Clone)]
pub struct ChangeService {
    store: Store,
    repos_dir: PathBuf,
    jj: Jj,
}

impl ChangeService {
    pub fn new(store: Store, repos_dir: impl Into<PathBuf>, jj: Jj) -> Self {
        Self {
            store,
            repos_dir: repos_dir.into(),
            jj,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn jj(&self) -> &Jj {
        &self.jj
    }

    pub fn repository(&self, repo: &str) -> Result<PathBuf> {
        repository_path(&self.repos_dir, repo)
    }

    pub fn list(&self) -> Result<Vec<Change>> {
        self.store.list()
    }

    pub fn create(
        &self,
        repo: &str,
        card: u64,
        title: Option<&str>,
        session: Option<&str>,
    ) -> Result<Change> {
        self.repository(repo)?;
        self.store.create(repo, card, title, session)
    }

    pub fn get(&self, repo: &str, card: u64) -> Result<Change> {
        let path = self.repository(repo)?;
        let mut change = self.store.require(repo, card)?;
        for round in &mut change.rounds {
            let resolved = self.jj.show(&path, &round.change_id)?;
            round.commit = resolved.commit;
            round.divergent = resolved.divergent;
        }
        change.path = Some(path.to_string_lossy().into_owned());
        Ok(change)
    }

    pub fn add_round(&self, repo: &str, card: u64, input: RoundInput) -> Result<Round> {
        let path = self.repository(repo)?;
        let jj = &self.jj;
        let change_id = input.change_id.clone();
        self.store.add_round(repo, card, input, |change| {
            let resolved = jj.show(&path, &change_id)?;
            if resolved.divergent {
                return Err(Error::Invalid(format!(
                    "change id {change_id} is divergent in {repo}"
                )));
            }
            let Some(commit) = resolved.commit else {
                return Err(Error::Invalid(format!(
                    "change id {change_id} does not exist in {repo}"
                )));
            };
            if let Some(previous) = change.rounds.last()
                && !commit.parents.contains(&previous.change_id)
            {
                return Err(Error::Invalid(format!(
                    "round {} must be a child of round {} ({}); {change_id} is not",
                    change.rounds.len() + 1,
                    previous.n,
                    previous.change_id
                )));
            }
            Ok(())
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
    ) -> Result<Annotation> {
        let repository = self.repository(repo)?;
        let jj = &self.jj;
        self.store
            .add_annotation(repo, card, round, path, line, side, text, |_, target| {
                let diff = parse_diff(&jj.diff_for_round(&repository, &target.change_id)?);
                if !diff.contains_anchor(path, side, line) {
                    return Err(Error::Invalid(format!(
                        "round {round} of {repo}/{card} has no {side:?} anchor at {path}:{line}"
                    )));
                }
                Ok(())
            })
    }

    pub fn round_diff(&self, repo: &str, card: u64, round: u32) -> Result<Diff> {
        let path = self.repository(repo)?;
        let change = self.store.require(repo, card)?;
        let target = change
            .rounds
            .iter()
            .find(|candidate| candidate.n == round)
            .ok_or_else(|| Error::NoRound {
                repo: repo.to_owned(),
                card,
                round,
            })?;
        Ok(parse_diff(
            &self.jj.diff_for_round(&path, &target.change_id)?,
        ))
    }

    pub fn round_diff_text(&self, repo: &str, card: u64, round: u32) -> Result<String> {
        let path = self.repository(repo)?;
        let change = self.store.require(repo, card)?;
        let target = change
            .rounds
            .iter()
            .find(|candidate| candidate.n == round)
            .ok_or_else(|| Error::NoRound {
                repo: repo.to_owned(),
                card,
                round,
            })?;
        self.jj.diff_for_round(&path, &target.change_id)
    }

    pub fn cumulative_diff(&self, repo: &str, card: u64) -> Result<Diff> {
        Ok(parse_diff(&self.cumulative_diff_text(repo, card)?))
    }

    pub fn cumulative_diff_text(&self, repo: &str, card: u64) -> Result<String> {
        let path = self.repository(repo)?;
        let change = self.store.require(repo, card)?;
        let first = change
            .rounds
            .first()
            .ok_or_else(|| Error::Invalid(format!("change {repo}/{card} has no rounds")))?;
        let last = change.rounds.last().expect("the first round exists");
        self.jj
            .diff_cumulative(&path, &first.change_id, &last.change_id)
    }

    pub fn set_session(&self, repo: &str, card: u64, session: &str) -> Result<Change> {
        self.repository(repo)?;
        self.store.set_session(repo, card, session)
    }

    pub fn transition(&self, repo: &str, card: u64, state: ChangeState) -> Result<Change> {
        self.repository(repo)?;
        self.store.transition(repo, card, state)
    }

    pub fn change_id(&self, repo: &str, revision: &str) -> Result<String> {
        let path = self.repository(repo)?;
        self.jj.change_id(&path, revision)
    }
}

pub fn default_change_dir() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/share")))
        .ok_or_else(|| {
            Error::Invalid("HOME or XDG_DATA_HOME is required for the change log".to_owned())
        })?;
    // Retain the shipped log location so the Rust cutover reads every active
    // change the bridge already authored.
    Ok(data.join("skiff-bridge/changes"))
}

pub fn default_repos_dir() -> Result<PathBuf> {
    std::env::var_os("DW_REPOS_DIR")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join("code")))
        .ok_or_else(|| {
            Error::Invalid("HOME or DW_REPOS_DIR is required to resolve repositories".to_owned())
        })
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
