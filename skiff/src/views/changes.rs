use anyhow::Result;
use change::{ChangeRef, ChangeService};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "gen/")]
pub struct ChangesView {
    pub changes: Vec<ChangeRef>,
}

pub fn compute(service: &ChangeService) -> Result<ChangesView> {
    Ok(ChangesView {
        changes: service
            .list()?
            .into_iter()
            .map(|change| change.reference())
            .collect(),
    })
}
