use anyhow::Result;
use change::{Change, ChangeService, ChangeState, Diff};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct ChangeView {
    pub change: Change,
    pub diff: Diff,
    pub round: Option<u32>,
    pub unfinished: Vec<String>,
    pub will_deploy: Option<u32>,
}

pub fn compute(
    service: &ChangeService,
    repo: &str,
    card: u64,
    round: Option<u32>,
) -> Result<ChangeView> {
    let change = service.get(repo, card)?;
    let diff = match round {
        Some(round) => service.round_diff(repo, card, round)?,
        None => service.cumulative_diff(repo, card)?,
    };
    let unfinished = unfinished(&change);
    Ok(ChangeView {
        change,
        diff,
        round,
        unfinished,
        will_deploy: None,
    })
}

fn unfinished(change: &Change) -> Vec<String> {
    if change.state != ChangeState::Shipped {
        return Vec::new();
    }
    let mut steps = Vec::new();
    if !change
        .record_export
        .as_ref()
        .is_some_and(|outcome| outcome.ok)
    {
        steps.push("record".to_owned());
    }
    if !change
        .card_comment
        .as_ref()
        .is_some_and(|outcome| outcome.ok)
    {
        steps.push("card comment".to_owned());
    }
    if change.deploy.as_ref().is_some_and(|deploy| {
        deploy.error.is_some()
            || deploy
                .services
                .iter()
                .any(|service| service.job_id.is_some() && service.outcome.is_none())
    }) {
        steps.push("deploy".to_owned());
    }
    steps
}
