//! The fleet settings store: today just the shared theme, persisted to a JSON
//! file so it survives restarts and is the single source of truth all services
//! read. `tide serve` is the only writer, so an in-memory copy behind a `Mutex`
//! plus a write-through to disk is enough — no database.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The fleet-wide colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Theme {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dark" => Ok(Theme::Dark),
            "light" => Ok(Theme::Light),
            _ => Err(()),
        }
    }
}

/// The persisted settings document (versioned for forward-compatibility).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: Theme::Dark }
    }
}

/// The settings store: an in-memory theme guarded by a `Mutex`, written through
/// to `settings.json` on every change.
pub struct Store {
    path: PathBuf,
    theme: Mutex<Theme>,
}

impl Store {
    /// Open the store under `data_dir`, loading the existing settings or
    /// defaulting to dark (and not yet writing a file — the first `set` does).
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join("settings.json");
        let theme = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default()
            .theme;
        Store { path, theme: Mutex::new(theme) }
    }

    pub fn get(&self) -> Theme {
        *self.theme.lock().expect("theme lock")
    }

    /// Set the theme and persist it. Returns an error string if the write fails
    /// (the in-memory value is still updated so the running fleet reflects it).
    pub fn set(&self, theme: Theme) -> Result<(), String> {
        *self.theme.lock().expect("theme lock") = theme;
        self.persist(theme)
    }

    /// Atomically write the settings file (temp file + rename in the same dir).
    fn persist(&self, theme: Theme) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&Settings { theme })
            .map_err(|e| format!("serializing settings: {e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("installing {}: {e}", self.path.display()))
    }
}
