//! APNs push, so a turn ending while the phone app is closed is not silent.
//!
//! Push is **optional and off until configured**. `~/.config/harness/push.toml`
//! is seeded on first run with everything harness already knows (team and
//! bundle id) and one blank to fill in — the key id from the APNs auth key you
//! download from Apple. Until that blank is filled and the `.p8` exists,
//! [Pusher::load] returns `None` and the server runs exactly as before. That
//! matters because the credential is not in this repo and never will be: a
//! deploy on a machine without it must not fail.
//!
//! Delivery is best-effort by design. A notification is a courtesy; the
//! transcript is the record. Nothing here can fail a turn, so every error is
//! logged and swallowed, and sending happens off the turn's task.
//!
//! Two APNs rules shape the code:
//!
//! 1. The authentication JWT must be refreshed **at least** hourly and **at
//!    most** every 20 minutes — reuse it too long and requests 403, mint it too
//!    often and Apple rate-limits you. It is cached for 45 minutes.
//! 2. A `410 Unregistered` (or `400 BadDeviceToken`) is the only reliable
//!    signal that an app was deleted, so those responses drop the token rather
//!    than being retried forever.

use crate::store::{Store, Write, Writer};
use crate::util::{home, now_secs, truncate_chars};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a minted JWT is reused. Apple's window is 20–60 minutes.
const TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

/// Body text is a preview, not the answer — the app has the whole thing.
const BODY_CAP: usize = 160;

const CONFIG_TEMPLATE: &str = r#"# APNs push for the Harness iOS app. Delete this file to disable push.
#
# To finish setting this up:
#   1. developer.apple.com -> Certificates, Identifiers & Profiles -> Identifiers
#      -> com.deepwa7er.Harness -> tick "Push Notifications" -> Save.
#   2. Keys -> + -> tick "Apple Push Notifications service (APNs)" -> Continue
#      -> Register -> Download. You get AuthKey_XXXXXXXXXX.p8, once only.
#   3. Save it as ~/.config/harness/apns.p8 and put the XXXXXXXXXX part in
#      key_id below.
#
# Until key_id is filled in and the key file exists, push stays off and harness
# logs one line saying so.

team_id = "{team_id}"
bundle_id = "{bundle_id}"
key_id = ""

# Path to the .p8 downloaded in step 2.
key_path = "~/.config/harness/apns.p8"

# Apple runs two APNs environments and a token minted for one is rejected by
# the other. A development build (Xcode "Run" onto a device) registers with the
# sandbox; TestFlight and App Store builds use production. Set true while
# building to your own phone from Xcode.
sandbox = true
"#;

#[derive(Deserialize)]
struct RawConfig {
    team_id: String,
    bundle_id: String,
    key_id: String,
    key_path: Option<String>,
    sandbox: Option<bool>,
}

/// A notification worth waking a phone for.
pub struct Notification {
    /// Session the notification belongs to — carried in the payload so a tap
    /// can open the right conversation, and used as the collapse id so a
    /// session never stacks up notifications.
    pub session: String,
    pub title: String,
    pub body: String,
}

impl Notification {
    /// The end of a turn, described by whatever it produced.
    pub fn turn_ended(session: &str, label: &str, answer: &str, fatal: Option<&str>) -> Notification {
        let body = match fatal {
            Some(error) => format!("Turn failed: {}", truncate_chars(error, BODY_CAP)),
            None => {
                let text = answer.trim();
                if text.is_empty() {
                    "Finished.".to_string()
                } else {
                    truncate_chars(&text.replace('\n', " "), BODY_CAP)
                }
            }
        };
        Notification { session: session.to_string(), title: label.to_string(), body }
    }

    /// The model asked something and cannot continue without an answer.
    pub fn asked(session: &str, label: &str, question: &str) -> Notification {
        Notification {
            session: session.to_string(),
            title: format!("{label} needs you"),
            body: truncate_chars(question.trim(), BODY_CAP),
        }
    }
}

struct CachedToken {
    jwt: String,
    minted_at: std::time::Instant,
}

pub struct Pusher {
    client: reqwest::Client,
    store: Arc<Store>,
    writer: Writer,
    team_id: String,
    bundle_id: String,
    key_id: String,
    key: EncodingKey,
    host: &'static str,
    cached: Mutex<Option<CachedToken>>,
}

impl Pusher {
    /// Load push configuration, seeding the config file on first run.
    ///
    /// `None` means "not configured" and is the expected state until the APNs
    /// key exists — the caller carries on without push.
    pub fn load(store: Arc<Store>, writer: Writer) -> Option<Pusher> {
        let path = config_path();
        if !path.exists() {
            seed_config(&path);
            return None;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                crate::log(&format!("[push] could not read {}: {e}", path.display()));
                return None;
            }
        };
        let config: RawConfig = match toml::from_str(&raw) {
            Ok(config) => config,
            Err(e) => {
                crate::log(&format!("[push] {} is not valid TOML: {e}", path.display()));
                return None;
            }
        };
        if config.key_id.trim().is_empty() {
            crate::log(&format!(
                "[push] disabled — key_id is blank in {} (see the comments there)",
                path.display()
            ));
            return None;
        }
        let key_file = expand(
            config.key_path.as_deref().unwrap_or("~/.config/harness/apns.p8"),
        );
        let pem = match std::fs::read(&key_file) {
            Ok(pem) => pem,
            Err(e) => {
                crate::log(&format!("[push] disabled — cannot read {}: {e}", key_file.display()));
                return None;
            }
        };
        let key = match EncodingKey::from_ec_pem(&pem) {
            Ok(key) => key,
            Err(e) => {
                crate::log(&format!(
                    "[push] disabled — {} is not a usable APNs key: {e}",
                    key_file.display()
                ));
                return None;
            }
        };
        let sandbox = config.sandbox.unwrap_or(true);
        let host = if sandbox {
            "https://api.sandbox.push.apple.com"
        } else {
            "https://api.push.apple.com"
        };
        crate::log(&format!(
            "[push] enabled — {} ({})",
            config.bundle_id,
            if sandbox { "sandbox" } else { "production" }
        ));
        Some(Pusher {
            // APNs is HTTP/2 only. reqwest negotiates h2 over ALPN with
            // rustls, which is what the fleet already uses.
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .ok()?,
            store,
            writer,
            team_id: config.team_id,
            bundle_id: config.bundle_id,
            key_id: config.key_id.trim().to_string(),
            key,
            host,
            cached: Mutex::new(None),
        })
    }

    /// Send to every registered device. Errors are logged, never propagated.
    pub async fn notify(&self, note: Notification) {
        let devices = match self.store.devices() {
            Ok(devices) => devices,
            Err(e) => {
                crate::log(&format!("[push] could not list devices: {e}"));
                return;
            }
        };
        if devices.is_empty() {
            return;
        }
        let jwt = match self.jwt() {
            Ok(jwt) => jwt,
            Err(e) => {
                crate::log(&format!("[push] could not sign a token: {e}"));
                return;
            }
        };
        let payload = json!({
            "aps": {
                "alert": { "title": note.title, "body": note.body },
                "sound": "default",
                "thread-id": note.session,
            },
            // Read by the app to open the right conversation on tap.
            "session": note.session,
        });
        for device in devices {
            self.send(&jwt, &device, &note.session, &payload).await;
        }
    }

    async fn send(&self, jwt: &str, device: &str, session: &str, payload: &serde_json::Value) {
        let url = format!("{}/3/device/{device}", self.host);
        let response = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &self.bundle_id)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            // One live notification per session: a new one replaces the old
            // rather than stacking up while you are away from the phone.
            .header("apns-collapse-id", truncate_chars(session, 64))
            .json(payload)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(e) => {
                crate::log(&format!("[push] send failed: {e}"));
                return;
            }
        };
        let status = response.status();
        if status.is_success() {
            return;
        }
        let detail = response.text().await.unwrap_or_default();
        // 410 Unregistered / 400 BadDeviceToken: the app is gone from this
        // device. Keeping the token would mean failing forever.
        if status == 410 || detail.contains("BadDeviceToken") || detail.contains("Unregistered") {
            crate::log("[push] dropping a device token APNs reports as gone");
            self.writer.send(Write::ForgetDevice { token: device.to_string() });
            return;
        }
        crate::log(&format!("[push] APNs {status}: {}", truncate_chars(&detail, 200)));
    }

    /// A cached ES256 token, minted if the old one is past [TOKEN_LIFETIME].
    fn jwt(&self) -> Result<String, String> {
        let mut cached = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = cached.as_ref()
            && current.minted_at.elapsed() < TOKEN_LIFETIME {
                return Ok(current.jwt.clone());
            }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            iat: u64,
        }
        let claims = Claims { iss: &self.team_id, iat: now_secs() };
        let jwt = jsonwebtoken::encode(&header, &claims, &self.key).map_err(|e| e.to_string())?;
        *cached = Some(CachedToken { jwt: jwt.clone(), minted_at: std::time::Instant::now() });
        Ok(jwt)
    }
}

fn config_path() -> PathBuf {
    home().join(".config").join("harness").join("push.toml")
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

/// Write the template so the setup steps live next to the thing they configure.
fn seed_config(path: &PathBuf) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body = CONFIG_TEMPLATE
        .replace("{team_id}", "7PF4JMJ3SH")
        .replace("{bundle_id}", "com.deepwa7er.Harness");
    match std::fs::write(path, body) {
        Ok(()) => crate::log(&format!(
            "[push] disabled — created {}; fill in key_id to enable",
            path.display()
        )),
        Err(e) => crate::log(&format!("[push] could not create {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_notification_prefers_the_answer() {
        let note = Notification::turn_ended("s1", "fleet", "  Fixed the auth lock.\nPushed.  ", None);
        assert_eq!(note.title, "fleet");
        assert_eq!(note.body, "Fixed the auth lock. Pushed.", "newlines flatten to one line");
        assert_eq!(note.session, "s1");
    }

    #[test]
    fn turn_notification_reports_a_failure_instead() {
        let note = Notification::turn_ended("s1", "fleet", "ignored", Some("network error"));
        assert!(note.body.starts_with("Turn failed: network error"), "{}", note.body);
    }

    #[test]
    fn a_turn_with_no_text_still_says_something() {
        let note = Notification::turn_ended("s1", "fleet", "   \n ", None);
        assert_eq!(note.body, "Finished.");
    }

    #[test]
    fn long_bodies_are_capped() {
        let note = Notification::turn_ended("s1", "fleet", &"x".repeat(1_000), None);
        assert_eq!(note.body.chars().count(), BODY_CAP);
    }

    #[test]
    fn ask_notification_says_it_needs_you() {
        let note = Notification::asked("s2", "lagoon", "Which branch should I target?");
        assert_eq!(note.title, "lagoon needs you");
        assert_eq!(note.body, "Which branch should I target?");
    }

    #[test]
    fn tilde_paths_expand_to_home() {
        assert_eq!(expand("~/.config/harness/apns.p8"), home().join(".config/harness/apns.p8"));
        assert_eq!(expand("/etc/apns.p8"), PathBuf::from("/etc/apns.p8"));
    }
}
