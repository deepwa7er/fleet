//! Shared location for Tugboat's local append-only operational records.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn tugboat_dir() -> Result<PathBuf> {
    resolve_tugboat_dir(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn resolve_tugboat_dir(xdg_data_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let base = match xdg_data_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = home.context("no HOME or XDG_DATA_HOME to locate the tugboat data dir")?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(base.join("tugboat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn xdg_data_home_wins_when_set() {
        assert_eq!(
            resolve_tugboat_dir(os("/xdg/data"), os("/home/u")).unwrap(),
            PathBuf::from("/xdg/data/tugboat")
        );
    }

    #[test]
    fn home_is_the_fallback_for_unset_or_empty_xdg() {
        let expected = PathBuf::from("/home/u/.local/share/tugboat");
        assert_eq!(resolve_tugboat_dir(None, os("/home/u")).unwrap(), expected);
        assert_eq!(
            resolve_tugboat_dir(os(""), os("/home/u")).unwrap(),
            expected
        );
    }

    #[test]
    fn missing_home_and_xdg_is_an_error() {
        assert!(resolve_tugboat_dir(None, None).is_err());
    }
}
