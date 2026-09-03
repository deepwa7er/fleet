//! Commands and live state for sessions owned by `opencode serve`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::{Mutex, broadcast};

use crate::ingest::Topic;
use crate::ingest::loop_services::now_ms;
use crate::ingest::opencode::{LiveEvent, OpencodeClient};
use crate::model::{Harness, Message, SessionKey};

use super::{LiveState, PendingPrompt};

#[derive(Default)]
struct State {
    live: LiveState,
    user_count_at_send: usize,
}

pub struct OpencodeRuns {
    client: Result<OpencodeClient, String>,
    topics: broadcast::Sender<Topic>,
    sessions: Mutex<HashMap<SessionKey, State>>,
}

impl OpencodeRuns {
    pub fn new(
        client: Result<OpencodeClient, String>,
        topics: broadcast::Sender<Topic>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            topics,
            sessions: Mutex::default(),
        })
    }

    pub async fn live(&self, session: &SessionKey) -> LiveState {
        self.sessions
            .lock()
            .await
            .get(session)
            .map(|state| state.live.clone())
            .unwrap_or_default()
    }

    /// Forward live observations from the ingest to this run state.
    ///
    /// The single home for the ingest→run half of the topic-driven wiring, so
    /// the composition root and the integration harness share it instead of
    /// each hand-rolling the loop. A lagged subscription heals on the next
    /// poll: every poll re-observes every session it sees.
    pub fn spawn_forwarding(
        self: &Arc<Self>,
        mut live: broadcast::Receiver<LiveEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let runs = self.clone();
        tokio::spawn(async move {
            loop {
                match live.recv().await {
                    Ok(LiveEvent::Observed(observation)) => {
                        runs
                            .observed(
                                observation.session,
                                observation.pending,
                                observation.users,
                            )
                            .await;
                    }
                    Ok(LiveEvent::Forgot(session)) => {
                        runs.forgot(&session).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    pub async fn send(&self, session: &SessionKey, text: &str, client_id: &str) -> Result<()> {
        opencode_only(session)?;
        if text.trim().is_empty() {
            bail!("an empty prompt has nothing to ask");
        }
        let client = self.client.clone().map_err(anyhow::Error::msg)?;
        if client.session(&session.local).await?.is_none() {
            bail!("session {session} not found");
        }
        // The command watermark comes from OpenCode itself, not Skiff's
        // eventually consistent read model. This prevents an older cached
        // transcript from retiring the pending bubble for the wrong prompt.
        let users = client
            .messages(&session.local)
            .await?
            .iter()
            .filter(|message| message["info"]["role"] == "user")
            .count();
        {
            let mut sessions = self.sessions.lock().await;
            let state = sessions.entry(session.clone()).or_default();
            if state.live.working {
                bail!("a run is already active for this session");
            }
            state.user_count_at_send = users;
            state.live.pending_prompt = Some(PendingPrompt {
                client_id: client_id.to_owned(),
                text: text.to_owned(),
                sent_ms: now_ms(),
            });
        }
        let _ = self.topics.send(Topic::Run(session.clone()));
        if let Err(error) = client.prompt(&session.local, text).await {
            self.clear_prompt(session).await;
            return Err(error).context("sending the prompt to OpenCode");
        }
        Ok(())
    }

    pub async fn abort(&self, session: &SessionKey) -> Result<()> {
        opencode_only(session)?;
        let client = self.client.clone().map_err(anyhow::Error::msg)?;
        if client.session(&session.local).await?.is_none() {
            bail!("session {session} not found");
        }
        client
            .abort(&session.local)
            .await
            .context("aborting OpenCode")
    }

    pub async fn rename(&self, session: &SessionKey, name: &str) -> Result<()> {
        opencode_only(session)?;
        let name = name.trim();
        if name.is_empty() {
            bail!("a session name cannot be empty");
        }
        let client = self.client.clone().map_err(anyhow::Error::msg)?;
        if client.session(&session.local).await?.is_none() {
            bail!("session {session} not found");
        }
        client
            .rename(&session.local, name)
            .await
            .context("renaming OpenCode")
    }

    /// Apply what the remote source just observed. This is the convergence
    /// point between OpenCode's message list and Skiff's command overlay.
    pub async fn observed(&self, session: SessionKey, pending: Option<Message>, users: usize) {
        let mut sessions = self.sessions.lock().await;
        let state = sessions.entry(session.clone()).or_default();
        let before = state.live.clone();
        state.live.pending = pending;
        state.live.working = state.live.pending.is_some();
        if state.live.pending_prompt.is_some() && users > state.user_count_at_send {
            state.live.pending_prompt = None;
        }
        let after = state.live.clone();
        if after == LiveState::default() {
            sessions.remove(&session);
        }
        drop(sessions);
        if before != after {
            let _ = self.topics.send(Topic::Run(session));
        }
    }

    pub async fn forgot(&self, session: &SessionKey) {
        if self.sessions.lock().await.remove(session).is_some() {
            let _ = self.topics.send(Topic::Run(session.clone()));
        }
    }

    async fn clear_prompt(&self, session: &SessionKey) {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(session) {
            state.live.pending_prompt = None;
            if state.live == LiveState::default() {
                sessions.remove(session);
            }
        }
        drop(sessions);
        let _ = self.topics.send(Topic::Run(session.clone()));
    }
}

fn opencode_only(session: &SessionKey) -> Result<()> {
    if session.harness != Harness::Opencode {
        bail!("{} is not an OpenCode session", session);
    }
    Ok(())
}
