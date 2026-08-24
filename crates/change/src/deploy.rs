//! Token-gated tugboat deploy client.
//!
//! Triggering is a consequence of landing. Reading the recorded jobs remains
//! a Skiff view concern; this client only speaks the daemon's write contract
//! and follows job IDs that the durable change log already owns.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::{DeployOutcome, DeployService, Error, Result};

#[derive(Debug, Clone)]
pub struct TugboatConfig {
    pub base: String,
    pub token: String,
    pub poll_interval: Duration,
    pub poll_deadline: Duration,
}

impl TugboatConfig {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("TUGBOAT_SERVE_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty())?;
        Some(Self {
            base: std::env::var("TUGBOAT_SERVE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:7878".to_owned()),
            token,
            poll_interval: Duration::from_secs(5),
            poll_deadline: Duration::from_secs(10 * 60),
        })
    }
}

#[derive(Clone)]
pub struct TugboatClient {
    http: reqwest::Client,
    config: TugboatConfig,
    services: Arc<tokio::sync::Mutex<Option<(tokio::time::Instant, usize)>>>,
}

pub type DeployFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Port named by DW-004. Landing depends on this contract, not tugboat's
/// transport; the HTTP implementation is one adapter.
pub trait DeployTrigger: Send + Sync {
    fn service_count(&self) -> DeployFuture<'_, usize>;
    fn trigger_all(&self) -> DeployFuture<'_, Vec<DeployService>>;
    fn job_outcome<'a>(&'a self, job_id: &'a str) -> DeployFuture<'a, Option<DeployOutcome>>;
    fn poll_interval(&self) -> Duration;
    fn poll_deadline(&self) -> Duration;
}

impl DeployTrigger for TugboatClient {
    fn service_count(&self) -> DeployFuture<'_, usize> {
        Box::pin(TugboatClient::service_count(self))
    }

    fn trigger_all(&self) -> DeployFuture<'_, Vec<DeployService>> {
        Box::pin(TugboatClient::trigger_all(self))
    }

    fn job_outcome<'a>(&'a self, job_id: &'a str) -> DeployFuture<'a, Option<DeployOutcome>> {
        Box::pin(TugboatClient::job_outcome(self, job_id))
    }

    fn poll_interval(&self) -> Duration {
        self.config.poll_interval
    }

    fn poll_deadline(&self) -> Duration {
        self.config.poll_deadline
    }
}

impl TugboatClient {
    pub fn new(config: TugboatConfig) -> Result<Self> {
        let base = config.base.trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::Invalid(format!(
                "tugboat base must be an absolute HTTP(S) URL: {:?}",
                config.base
            )));
        }
        if config.token.trim().is_empty() {
            return Err(Error::Invalid("tugboat token must not be empty".to_owned()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| Error::External(format!("building tugboat client: {error}")))?;
        Ok(Self {
            http,
            config: TugboatConfig {
                base: base.to_owned(),
                ..config
            },
            services: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub async fn service_count(&self) -> Result<usize> {
        let mut cache = self.services.lock().await;
        if let Some((at, count)) = *cache
            && at.elapsed() < Duration::from_secs(60)
        {
            return Ok(count);
        }
        let services: Vec<Service> = self.get_json("/services").await?;
        let count = services.len();
        *cache = Some((tokio::time::Instant::now(), count));
        Ok(count)
    }

    pub async fn trigger_all(&self) -> Result<Vec<DeployService>> {
        let response = self
            .http
            .post(format!("{}/deploy", self.config.base))
            .bearer_auth(&self.config.token)
            .send()
            .await
            .map_err(|error| self.request_error("POST /deploy", error))?;
        let response = self.require_success("POST /deploy", response).await?;
        let started: FleetDeploy = response
            .json()
            .await
            .map_err(|error| Error::External(format!("parsing tugboat POST /deploy: {error}")))?;
        Ok(started
            .jobs
            .into_iter()
            .map(|job| DeployService {
                name: job.name,
                job_id: job.job_id.clone(),
                status: if job.job_id.is_some() {
                    "started".to_owned()
                } else if job.status.as_deref() == Some("in_progress") {
                    "in_progress".to_owned()
                } else {
                    "not_started".to_owned()
                },
                outcome: None,
            })
            .collect())
    }

    pub async fn job_outcome(&self, job_id: &str) -> Result<Option<DeployOutcome>> {
        if job_id.is_empty() {
            return Err(Error::Invalid("deploy job id must not be empty".to_owned()));
        }
        let status: JobStatus = self
            .get_json(&format!("/jobs/{}", url_segment(job_id)))
            .await?;
        Ok(status.outcome.map(|outcome| DeployOutcome {
            ok: outcome.ok,
            message: outcome.error,
        }))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .get(format!("{}{}", self.config.base, path))
            .bearer_auth(&self.config.token)
            .send()
            .await
            .map_err(|error| self.request_error(&format!("GET {path}"), error))?;
        self.require_success(&format!("GET {path}"), response)
            .await?
            .json()
            .await
            .map_err(|error| Error::External(format!("parsing tugboat GET {path}: {error}")))
    }

    async fn require_success(
        &self,
        operation: &str,
        response: reqwest::Response,
    ) -> Result<reqwest::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(Error::External(format!(
            "tugboat {operation} answered {status}: {}",
            body.chars().take(500).collect::<String>()
        )))
    }

    fn request_error(&self, operation: &str, error: reqwest::Error) -> Error {
        Error::External(format!(
            "tugboat daemon unreachable at {} during {operation}: {error}",
            self.config.base
        ))
    }
}

fn url_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Deserialize)]
struct Service {
    #[allow(dead_code)]
    name: String,
}

#[derive(Deserialize)]
struct FleetDeploy {
    #[serde(default)]
    jobs: Vec<FleetJob>,
}

#[derive(Deserialize)]
struct FleetJob {
    name: String,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct JobStatus {
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    outcome: Option<JobOutcome>,
}

#[derive(Deserialize)]
struct JobOutcome {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(url_segment("job/a b"), "job%2Fa%20b");
    }

    #[test]
    fn malformed_config_is_rejected() {
        let mut config = TugboatConfig {
            base: "localhost:1".to_owned(),
            token: "token".to_owned(),
            poll_interval: Duration::from_secs(1),
            poll_deadline: Duration::from_secs(1),
        };
        assert!(TugboatClient::new(config.clone()).is_err());
        config.base = "http://localhost:1".to_owned();
        config.token.clear();
        assert!(TugboatClient::new(config).is_err());
    }
}
