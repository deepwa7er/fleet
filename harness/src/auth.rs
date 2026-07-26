//! Credentials: a KIMI_API_KEY env var, or reuse of the Kimi Code CLI's OAuth
//! login from ~/.kimi-code/credentials/kimi-code.json with self-serve refresh.
//!
//! Access tokens live ~15 minutes. On expiry (and once on any mid-session 401)
//! the refresh token is POSTed to the OAuth endpoint and the rotated pair is
//! written back to the shared credentials file atomically (tmp → fsync →
//! rename, 0600), so the CLI keeps using it too. The file is shared without
//! locking; concurrent refreshes (CLI, REPL, web sessions) self-heal through
//! [OAuthCred::reload_if_changed] on the next 401.

use crate::util::{now_secs, truncate_chars};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const OAUTH_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_DEFAULT_HOST: &str = "https://auth.kimi.com";

const RELOGIN_HINT: &str = "Run `kimi` to log in again, or set KIMI_API_KEY.";

pub enum Auth {
    ApiKey(String),
    OAuth(OAuthCred),
}

pub struct OAuthCred {
    path: PathBuf,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: f64,
    expires_in: Option<u64>,
    raw: Value, // full credentials JSON; unknown fields are preserved on save
}

impl Auth {
    /// KIMI_API_KEY wins; otherwise reuse the Kimi Code CLI's OAuth credentials,
    /// refreshing them ourselves when they expire.
    pub async fn load(client: &reqwest::Client) -> Result<Auth, String> {
        if let Ok(key) = std::env::var("KIMI_API_KEY")
            && !key.is_empty() {
                return Ok(Auth::ApiKey(key));
            }
        let home = std::env::var("KIMI_CODE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| crate::util::home().join(".kimi-code"));
        let cred = home.join("credentials").join("kimi-code.json");
        if cred.exists() {
            if let Some(mut oauth) = OAuthCred::from_file(&cred) {
                if oauth.expiring() {
                    crate::log("[auth] access token expired — refreshing via refresh_token");
                    oauth.refresh(client).await.map_err(|e| format!("{e}\n{RELOGIN_HINT}"))?;
                }
                return Ok(Auth::OAuth(oauth));
            }
            crate::log(&format!("[auth] could not parse {}", cred.display()));
        }
        Err("No credentials. Either:\n  \
             - create an API key in the Kimi Code Console and `export KIMI_API_KEY=...`, or\n  \
             - log in with the Kimi Code CLI (`kimi`, then /login) and rerun."
            .to_string())
    }

    /// A token valid for at least the next minute; refreshes if needed.
    pub async fn token(&mut self, client: &reqwest::Client) -> Result<String, String> {
        match self {
            Auth::ApiKey(key) => Ok(key.clone()),
            Auth::OAuth(cred) => {
                if cred.expiring() {
                    crate::log("[auth] token near expiry — refreshing");
                    cred.refresh(client).await.map_err(|e| format!("{e}\n{RELOGIN_HINT}"))?;
                }
                Ok(cred.access_token.clone())
            }
        }
    }

    /// Second chance after a 401: maybe the CLI refreshed the file on disk,
    /// otherwise refresh ourselves. Returns true if a retry is worthwhile.
    pub async fn handle_401(&mut self, client: &reqwest::Client) -> bool {
        match self {
            Auth::ApiKey(_) => false,
            Auth::OAuth(cred) => {
                if cred.reload_if_changed() {
                    crate::log("[auth] 401 — picked up newer credentials from disk");
                    return true;
                }
                crate::log("[auth] 401 — attempting token refresh");
                cred.refresh(client).await.is_ok()
            }
        }
    }
}

impl OAuthCred {
    fn from_file(path: &Path) -> Option<OAuthCred> {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        Some(OAuthCred {
            path: path.to_path_buf(),
            access_token: raw["access_token"].as_str()?.to_string(),
            refresh_token: raw["refresh_token"].as_str().map(str::to_string),
            expires_at: raw["expires_at"].as_f64().unwrap_or(0.0),
            expires_in: raw["expires_in"].as_u64(),
            raw,
        })
    }

    fn expiring(&self) -> bool {
        self.expires_at <= now_secs() as f64 + 60.0
    }

    fn reload_if_changed(&mut self) -> bool {
        if let Some(fresh) = Self::from_file(&self.path)
            && fresh.access_token != self.access_token && !fresh.expiring() {
                *self = fresh;
                return true;
            }
        false
    }

    /// Standard OAuth refresh against the Kimi auth server. The server rotates
    /// the refresh token, so the new pair is persisted back to the shared
    /// credentials file (atomically) for the CLI to pick up as well.
    async fn refresh(&mut self, client: &reqwest::Client) -> Result<(), String> {
        let rt = self
            .refresh_token
            .clone()
            .ok_or_else(|| "no refresh_token in credentials file".to_string())?;
        let host = std::env::var("KIMI_CODE_OAUTH_HOST")
            .ok()
            .or_else(|| std::env::var("KIMI_OAUTH_HOST").ok())
            .unwrap_or_else(|| OAUTH_DEFAULT_HOST.to_string());
        let url = format!("{}/api/oauth/token", host.trim_end_matches('/'));
        let resp = client
            .post(&url)
            .header("User-Agent", "kimi-harness/0.2")
            .timeout(Duration::from_secs(30))
            .form(&[
                ("client_id", OAUTH_CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", rt.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("OAuth refresh network error: {e}"))?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("OAuth refresh failed (HTTP {code}): {}", truncate_chars(&body, 300)));
        }
        let data: Value = resp.json().await.map_err(|e| format!("bad OAuth refresh response: {e}"))?;
        let access = data["access_token"].as_str().ok_or("refresh response missing access_token")?;
        let new_rt =
            data["refresh_token"].as_str().ok_or("refresh response missing refresh_token")?;
        let expires_in = data["expires_in"]
            .as_f64()
            .filter(|v| *v > 0.0)
            .ok_or("refresh response missing/invalid expires_in")?;
        self.access_token = access.to_string();
        self.refresh_token = Some(new_rt.to_string());
        self.expires_in = Some(expires_in as u64);
        self.expires_at = now_secs() as f64 + expires_in;
        if let Some(s) = data["scope"].as_str() {
            self.raw["scope"] = json!(s);
        }
        if let Some(s) = data["token_type"].as_str() {
            self.raw["token_type"] = json!(s);
        }
        self.save()?;
        crate::log("[auth] OAuth token refreshed");
        Ok(())
    }

    /// Write the updated credentials back: tmp file (0600) → fsync → rename.
    fn save(&mut self) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        self.raw["access_token"] = json!(self.access_token);
        if let Some(rt) = &self.refresh_token {
            self.raw["refresh_token"] = json!(rt);
        }
        self.raw["expires_at"] = json!(self.expires_at as u64);
        if let Some(ei) = self.expires_in {
            self.raw["expires_in"] = json!(ei);
        }
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "kimi-code.json".to_string());
        let tmp = self.path.with_file_name(format!("{name}.tmp"));
        let body = serde_json::to_string(&self.raw).map_err(|e| e.to_string())?;
        (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })()
        .map_err(|e| format!("could not save {}: {e}", self.path.display()))
    }
}
