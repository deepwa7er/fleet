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

    /// Atomically and durably write the settings file: write a sibling temp file,
    /// fsync it, rename it over the destination, then fsync the directory. A crash
    /// leaves either the old file or the fully-written new one — never a truncated
    /// file or a rename the directory entry lost.
    fn persist(&self, theme: Theme) -> Result<(), String> {
        use std::io::Write;

        let json = serde_json::to_string_pretty(&Settings { theme })
            .map_err(|e| format!("serializing settings: {e}"))?;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| "settings path has no parent directory".to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut file =
                std::fs::File::create(&tmp).map_err(|e| format!("creating {}: {e}", tmp.display()))?;
            file.write_all(json.as_bytes())
                .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
            file.sync_all()
                .map_err(|e| format!("fsync {}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("installing {}: {e}", self.path.display()))?;
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| format!("fsync directory {}: {e}", dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tide-storetest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn set_persists_across_reopen() {
        let dir = temp_dir();

        // Fresh store defaults to dark and writes nothing until the first set.
        let store = Store::open(&dir);
        assert_eq!(store.get(), Theme::Dark);
        assert!(!dir.join("settings.json").exists());

        // A set is durably written and read back by a fresh store.
        store.set(Theme::Light).unwrap();
        assert!(dir.join("settings.json").exists());
        assert_eq!(Store::open(&dir).get(), Theme::Light);

        // A subsequent set overwrites cleanly (temp file is not left behind).
        store.set(Theme::Dark).unwrap();
        assert_eq!(Store::open(&dir).get(), Theme::Dark);
        assert!(!dir.join("settings.json.tmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
