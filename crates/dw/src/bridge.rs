//! The bridge client — dw's window onto the change objects the skiff
//! bridge owns (DW-002 §4–6). Thin by design: every endpoint is one call,
//! every failure carries the bridge's own error text, and dw adds no
//! semantics the bridge does not already have.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub struct Bridge {
    base: String,
    password: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct Change {
    pub repo: String,
    pub card: u64,
    pub title: Option<String>,
    pub state: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub rounds: Vec<Round>,
    #[serde(rename = "lastLanding", default)]
    pub last_landing: Option<Landing>,
    #[serde(default)]
    pub landed: Option<Landed>,
}

// Rounds surface in dw only as a count, and a landing only as its reason
// (state carries the verdict); deserialize exactly that much.
#[derive(Debug, Deserialize)]
pub struct Round {}

#[derive(Debug, Deserialize)]
pub struct Landing {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Landed {
    pub tip: String,
}

#[derive(Deserialize)]
struct ChangeList {
    changes: Vec<Change>,
}

#[derive(Deserialize)]
struct BridgeError {
    error: String,
}

impl Bridge {
    pub fn new(password: String) -> Self {
        let base = std::env::var("SKIFF_BRIDGE_URL").unwrap_or_else(|_| "http://127.0.0.1:4120".into());
        Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            http: reqwest::Client::new(),
        }
    }

    pub async fn changes(&self) -> Result<Vec<Change>> {
        Ok(self.get::<ChangeList>("/change").await?.changes)
    }

    pub async fn change(&self, repo: &str, card: u64) -> Result<Change> {
        self.get(&format!("/change/{repo}/{card}")).await
    }

    pub async fn create_change(&self, repo: &str, card: u64, title: &str) -> Result<Change> {
        self.post(
            "/change",
            Some(serde_json::json!({ "repo": repo, "card": card, "title": title })),
        )
        .await
    }

    pub async fn add_round(&self, repo: &str, card: u64, author: &str, change_id: &str) -> Result<Round> {
        self.post(
            &format!("/change/{repo}/{card}/round"),
            Some(serde_json::json!({ "author": author, "changeId": change_id })),
        )
        .await
    }

    pub async fn submit(&self, repo: &str, card: u64) -> Result<Change> {
        self.post(&format!("/change/{repo}/{card}/submit"), None).await
    }

    pub async fn approve(&self, repo: &str, card: u64) -> Result<Change> {
        self.post(&format!("/change/{repo}/{card}/approve"), None).await
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .basic_auth("skiff", Some(&self.password))
            .send()
            .await
            .context("the skiff bridge is unreachable — is skiff-bridge running?")?;
        Self::parse(response).await
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: Option<serde_json::Value>) -> Result<T> {
        let mut request = self
            .http
            .post(format!("{}{path}", self.base))
            .basic_auth("skiff", Some(&self.password));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .context("the skiff bridge is unreachable — is skiff-bridge running?")?;
        Self::parse(response).await
    }

    // A bridge refusal carries a reason worth reading ({"error": …});
    // surface it verbatim rather than a status code.
    async fn parse<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        let bytes = response.bytes().await.context("reading the bridge response")?;
        if !status.is_success() {
            match serde_json::from_slice::<BridgeError>(&bytes) {
                Ok(err) => bail!("{}", err.error),
                Err(_) => bail!("the bridge answered HTTP {status}"),
            }
        }
        serde_json::from_slice(&bytes).context("the bridge answered a shape dw does not know")
    }
}
