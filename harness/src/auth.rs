//! Credentials: a KIMI_API_KEY env var, or reuse of the Kimi Code CLI's OAuth
//! login from ~/.kimi-code/credentials/kimi-code.json with self-serve refresh.
//!
//! Access tokens live ~15 minutes. On expiry (and on any mid-session 401) the
//! refresh token is POSTed to the OAuth endpoint and the rotated pair is
//! written back to the shared credentials file atomically (tmp → fsync →
//! rename, 0600), so the CLI keeps using it too.
//!
//! The whole read → refresh → write cycle runs under an advisory lock on a
//! sibling `.lock` file ([CredLock]), because the server *rotates* the refresh
//! token: two harness processes racing would have the loser POST a token the
//! winner already spent, and their saves could interleave into a spliced
//! credentials file that breaks the CLI login too. Under the lock the file is
//! re-read first, so a process that waited adopts the winner's result instead
//! of duplicating — or invalidating — it.

use crate::util::{now_secs, truncate_chars};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const OAUTH_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_DEFAULT_HOST: &str = "https://auth.kimi.com";

const RELOGIN_HINT: &str = "Run `kimi` to log in again, or set KIMI_API_KEY.";

/// How long to wait for the credentials lock before giving up. A healthy
/// holder releases within its OAuth request timeout (30s) and a crashed one
/// releases immediately (the kernel drops flock when the fd closes), so
/// exceeding this means a process is wedged while holding it — better to fail
/// with that diagnosis than to hang a turn forever.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(100);

/// Why we are refreshing — it decides whether an unchanged, unexpired token
/// already on disk is an acceptable outcome.
#[derive(Clone, Copy, PartialEq)]
enum RefreshReason {
    /// The access token is at or near expiry. Any usable token will do.
    Expiring,
    /// The server rejected the current access token (401). Adopting the same
    /// token back off disk would just fail again — only a *different* token,
    /// or a fresh network refresh, resolves it.
    Rejected,
}

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
                    oauth
                        .refresh(client, RefreshReason::Expiring)
                        .await
                        .map_err(|e| format!("{e}\n{RELOGIN_HINT}"))?;
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
                    cred.refresh(client, RefreshReason::Expiring)
                        .await
                        .map_err(|e| format!("{e}\n{RELOGIN_HINT}"))?;
                }
                Ok(cred.access_token.clone())
            }
        }
    }

    /// Second chance after a 401: maybe another process refreshed the file on
    /// disk, otherwise refresh ourselves. [OAuthCred::refresh] picks between
    /// those under the lock. Returns true if a retry is worthwhile.
    pub async fn handle_401(&mut self, client: &reqwest::Client) -> bool {
        match self {
            Auth::ApiKey(_) => false,
            Auth::OAuth(cred) => {
                crate::log("[auth] 401 — reloading or refreshing credentials");
                cred.refresh(client, RefreshReason::Rejected).await.is_ok()
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

    /// Bring the credentials up to date, holding the shared lock across the
    /// whole read → refresh → write cycle.
    ///
    /// The re-read under the lock is the point: whoever held it before us may
    /// have rotated the pair, which both makes our in-memory refresh token
    /// spent and makes their new access token good enough to reuse. So we
    /// adopt the file first and only POST when that leaves us short — which
    /// also collapses a burst of concurrent expiries into one network refresh.
    async fn refresh(
        &mut self,
        client: &reqwest::Client,
        reason: RefreshReason,
    ) -> Result<(), String> {
        // Must outlive the save() below — a plain `_` would drop it here.
        let _lock = CredLock::acquire(&self.path).await?;
        if let Some(fresh) = Self::from_file(&self.path) {
            // Disk is authoritative: our in-memory copy only ever diverges
            // from it by a save() we already completed, or one that failed
            // and propagated its error.
            let changed = fresh.access_token != self.access_token;
            *self = fresh;
            if disk_is_enough(reason, self.expiring(), changed) {
                if changed {
                    crate::log("[auth] adopted credentials refreshed by another process");
                }
                return Ok(());
            }
        }
        self.refresh_locked(client).await
    }

    /// Standard OAuth refresh against the Kimi auth server. The server rotates
    /// the refresh token, so the new pair is persisted back to the shared
    /// credentials file (atomically) for the CLI to pick up as well.
    ///
    /// Caller must hold the [CredLock]; [OAuthCred::refresh] is the entry point.
    async fn refresh_locked(&mut self, client: &reqwest::Client) -> Result<(), String> {
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
    ///
    /// The tmp name carries our pid. Callers hold the [CredLock], which
    /// serializes harness against harness, but the Kimi CLI takes no such lock
    /// — a shared tmp path would let its save and ours truncate and write the
    /// same file concurrently, renaming a spliced result into place. A
    /// per-process name keeps the worst case at "last rename wins", which
    /// leaves a valid file either way.
    fn save(&mut self) -> Result<(), String> {
        self.raw["access_token"] = json!(self.access_token);
        if let Some(rt) = &self.refresh_token {
            self.raw["refresh_token"] = json!(rt);
        }
        self.raw["expires_at"] = json!(self.expires_at as u64);
        if let Some(ei) = self.expires_in {
            self.raw["expires_in"] = json!(ei);
        }
        let tmp = sibling(&self.path, &format!(".{}.tmp", std::process::id()));
        let body = serde_json::to_string(&self.raw).map_err(|e| e.to_string())?;
        let written = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if written.is_err() {
            std::fs::remove_file(&tmp).ok(); // don't leave a half-written token behind
        }
        written.map_err(|e| format!("could not save {}: {e}", self.path.display()))
    }
}

/// Whether credentials just re-read from disk settle the matter, or we still
/// owe a network refresh. `changed` is whether the file's access token differs
/// from the one we arrived with.
fn disk_is_enough(reason: RefreshReason, expiring: bool, changed: bool) -> bool {
    match reason {
        // Any unexpired token will do, including our own unchanged one.
        RefreshReason::Expiring => !expiring,
        // The server rejected the token we came in with, so reading that same
        // token back is no help — only a different one is.
        RefreshReason::Rejected => !expiring && changed,
    }
}

/// A path next to `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_else(|| OsStr::new("kimi-code.json")).to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// An advisory `flock` serializing credential refreshes between processes,
/// released by the kernel when the fd closes — including on a crash, so it
/// cannot go stale.
///
/// The lock lives in a sibling `.lock` file rather than the credentials file
/// itself: [OAuthCred::save] replaces that path via rename, so a lock taken on
/// it would guard an inode the next process never opens. A dedicated path is
/// stable for the lock's whole lifetime.
///
/// This serializes harness against harness — the Kimi CLI does not participate.
/// What protects that case is narrower: the per-process tmp name in [save], and
/// the re-read under the lock that keeps us from POSTing a rotated-away token.
#[derive(Debug)]
struct CredLock {
    /// Held open for the lock's lifetime — the kernel releases the flock when
    /// this file closes, so the guard's only job is to stay alive.
    _file: std::fs::File,
}

impl CredLock {
    /// Acquire the lock guarding `cred_path`, waiting up to [LOCK_TIMEOUT].
    async fn acquire(cred_path: &Path) -> Result<CredLock, String> {
        let path = sibling(cred_path, ".lock");
        tokio::task::spawn_blocking(move || lock_blocking(&path, LOCK_TIMEOUT))
            .await
            .map_err(|e| format!("credential lock task failed: {e}"))?
    }
}

/// Blocking half of [CredLock::acquire]: poll `LOCK_EX | LOCK_NB` until the
/// lock is ours or `timeout` elapses. Non-blocking + poll rather than a
/// blocking `LOCK_EX` so a wedged holder surfaces as an error instead of
/// hanging the turn that needed a token.
fn lock_blocking(path: &Path, timeout: Duration) -> Result<CredLock, String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("could not open credential lock {}: {e}", path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        // Safe: `file` owns the fd for the duration of the call, and flock on a
        // valid fd has no preconditions beyond that.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(CredLock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("could not lock {}: {err}", path.display()));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for the credential lock {} — \
                 another process may be stuck holding it",
                timeout.as_secs_f32().round(),
                path.display()
            ));
        }
        thread::sleep(LOCK_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch directory, unique per test and per process.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-auth-{}-{tag}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_cred(path: &Path, access: &str, refresh: &str, expires_at: u64) {
        let body = json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_at": expires_at,
            "custom_cli_field": "keep me",
        });
        std::fs::write(path, serde_json::to_string(&body).unwrap()).unwrap();
    }

    #[test]
    fn lock_excludes_a_second_holder_and_frees_on_drop() {
        let dir = scratch("lock");
        let cred = dir.join("kimi-code.json");
        // flock is per open-file-description, so a second open() in this same
        // process contends exactly like another process would.
        let held = lock_blocking(&cred, Duration::from_millis(50)).unwrap();
        let err = lock_blocking(&cred, Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        drop(held);
        lock_blocking(&cred, Duration::from_millis(50)).expect("free once released");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_is_atomic_preserves_unknown_fields_and_leaves_no_tmp() {
        let dir = scratch("save");
        let cred = dir.join("kimi-code.json");
        write_cred(&cred, "old-access", "old-refresh", 1);
        let mut c = OAuthCred::from_file(&cred).unwrap();
        c.access_token = "new-access".into();
        c.refresh_token = Some("new-refresh".into());
        c.save().unwrap();

        let back: Value = serde_json::from_str(&std::fs::read_to_string(&cred).unwrap()).unwrap();
        assert_eq!(back["access_token"], "new-access");
        assert_eq!(back["refresh_token"], "new-refresh");
        // Fields harness doesn't model belong to the CLI; they must survive.
        assert_eq!(back["custom_cli_field"], "keep me");

        let litter: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(litter.is_empty(), "left tmp files behind: {litter:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tmp_path_is_process_unique() {
        let tmp = sibling(Path::new("/creds/kimi-code.json"), &format!(".{}.tmp", 4321));
        assert_eq!(tmp, PathBuf::from("/creds/kimi-code.json.4321.tmp"));
        assert_eq!(
            sibling(Path::new("/creds/kimi-code.json"), ".lock"),
            PathBuf::from("/creds/kimi-code.json.lock")
        );
    }

    /// The race this whole change exists to close: our token expired, but
    /// another process refreshed while we waited for the lock. We must adopt
    /// its result — POSTing our own (now rotated away) refresh token would
    /// spend a dead credential. Reaching the network here fails the test,
    /// which is the assertion: adoption happens before any request.
    #[tokio::test]
    async fn refresh_adopts_what_another_process_already_wrote() {
        let dir = scratch("adopt");
        let cred = dir.join("kimi-code.json");
        write_cred(&cred, "expired-access", "spent-refresh", 1);
        let mut c = OAuthCred::from_file(&cred).unwrap();
        assert!(c.expiring());

        // Another process wins the lock and rotates the pair.
        write_cred(&cred, "winner-access", "winner-refresh", now_secs() + 3600);

        c.refresh(&reqwest::Client::new(), RefreshReason::Expiring).await.unwrap();
        assert_eq!(c.access_token, "winner-access");
        assert_eq!(c.refresh_token.as_deref(), Some("winner-refresh"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn disk_is_enough_distinguishes_expiry_from_rejection() {
        use RefreshReason::{Expiring, Rejected};
        // Expiring: any unexpired token settles it, even our own unchanged one
        // (another process may have simply written the same token back).
        assert!(disk_is_enough(Expiring, false, false));
        assert!(disk_is_enough(Expiring, false, true));
        assert!(!disk_is_enough(Expiring, true, true));
        // Rejected: reading back the very token the server refused would just
        // repeat the failed request — only a different, unexpired one helps.
        assert!(!disk_is_enough(Rejected, false, false));
        assert!(disk_is_enough(Rejected, false, true));
        assert!(!disk_is_enough(Rejected, true, true));
    }
}
