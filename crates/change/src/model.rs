use serde::{Deserialize, Serialize};

use crate::Commit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Working,
    InReview,
    Landing,
    Shipped,
}

impl std::fmt::Display for ChangeState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Working => "working",
            Self::InReview => "in_review",
            Self::Landing => "landing",
            Self::Shipped => "shipped",
        })
    }
}

impl ChangeState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Working, Self::InReview)
                | (Self::InReview, Self::Working | Self::Landing)
                | (Self::Landing, Self::Shipped | Self::InReview)
        )
    }

    pub fn is_appendable(self) -> bool {
        matches!(self, Self::Working | Self::InReview)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Agent,
    You,
}

impl std::fmt::Display for Author {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Agent => "agent",
            Self::You => "you",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationSide {
    Old,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub side: AnnotationSide,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Round {
    pub n: u32,
    pub author: Author,
    pub change_id: String,
    pub note: Option<String>,
    #[serde(default)]
    pub gates_ran: Vec<String>,
    #[serde(default)]
    pub worth_knowing: Vec<String>,
    pub created_at: String,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<Commit>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub divergent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub note: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Landed {
    pub tip: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Landing {
    pub ok: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardComment {
    pub ok: bool,
    pub message: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordExport {
    pub ok: bool,
    pub message: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployOutcome {
    pub ok: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployService {
    pub name: String,
    pub job_id: Option<String>,
    pub status: String,
    #[serde(default)]
    pub outcome: Option<DeployOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deploy {
    pub at: String,
    pub error: Option<String>,
    #[serde(default)]
    pub services: Vec<DeployService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub repo: String,
    pub card: u64,
    pub title: Option<String>,
    pub session: Option<String>,
    pub state: ChangeState,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub rounds: Vec<Round>,
    pub last_request: Option<Request>,
    pub landed: Option<Landed>,
    pub last_landing: Option<Landing>,
    pub card_comment: Option<CardComment>,
    pub record_export: Option<RecordExport>,
    pub deploy: Option<Deploy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Change {
    pub fn reference(&self) -> ChangeRef {
        ChangeRef {
            repo: self.repo.clone(),
            card: self.card,
            state: self.state,
            rounds: self.rounds.len() as u32,
            title: self.title.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRef {
    pub repo: String,
    pub card: u64,
    pub state: ChangeState,
    pub rounds: u32,
    pub title: Option<String>,
    pub updated_at: String,
}
