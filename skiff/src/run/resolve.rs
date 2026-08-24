//! Finding a harness executable.
//!
//! **Why not just the bare name?** skiffd runs under a systemd user unit,
//! whose `PATH` is `/usr/local/bin:/usr/bin` — and pi, muse, and jj all
//! install into `~/.local/bin` or `~/.cargo/bin`. A bare name resolves fine in
//! an interactive shell and fails under the service, which is the worst
//! possible split: it works everywhere you test it by hand and nowhere it
//! actually runs. The Node bridge hit this and solved it the same way; this is
//! that discipline carried across.
//!
//! **Resolution happens once, at startup**, so a missing binary is discovered
//! when the service starts rather than on the first prompt someone sends.
//!
//! Unlike the bridge, a missing executable is **not fatal here**. skiffd reads
//! sessions from files, so it is still useful with no harness installed at all
//! — it simply cannot *run* one. The failure is carried until something tries
//! to, and then reported with everything needed to fix it (DW-004 §4: degrade
//! to a named error, never a dead service).

use std::path::{Path, PathBuf};

/// Where a CLI installs itself, beyond `PATH`.
fn home_candidates(name: &str) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join(".local/bin").join(name),
        home.join("bin").join(name),
        // Where `cargo install` puts binaries — present in interactive PATHs
        // but not systemd's, exactly like ~/.local/bin.
        home.join(".cargo/bin").join(name),
    ]
}

/// Resolve `requested` to an absolute path.
///
/// A request carrying a path separator is an explicit override and is used
/// as-is if it exists. A bare name is searched on `PATH`, then in the
/// home-relative locations above.
///
/// The error names every location tried, because "not found" without "where I
/// looked" is not something anyone can act on — which is precisely how this
/// surfaced in the first place.
pub fn binary(requested: &Path) -> Result<PathBuf, String> {
    let name = requested.to_string_lossy().into_owned();

    if requested.components().count() > 1 {
        return if requested.is_file() {
            Ok(requested.to_path_buf())
        } else {
            Err(format!("{name} does not exist"))
        };
    }

    let mut tried = Vec::new();
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }
    for candidate in home_candidates(&name) {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    Err(format!(
        "{name} was not found. Tried: {}. Set SKIFF_{}_BINARY to its absolute path.",
        tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        name.to_uppercase(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `PATH` and `HOME` are process-global, so these run under one lock rather
    /// than racing each other.
    fn with_env<T>(path: &str, home: &Path, f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: the lock serialises every mutation and read in this module's
        // tests, and nothing else in the suite reads PATH or HOME.
        unsafe {
            std::env::set_var("PATH", path);
            std::env::set_var("HOME", home);
        }
        f()
    }

    #[test]
    fn a_bare_name_is_found_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("usr/bin/pi");
        executable(&bin);
        let found = with_env(&dir.path().join("usr/bin").to_string_lossy(), dir.path(), || {
            binary(Path::new("pi"))
        });
        assert_eq!(found.unwrap(), bin);
    }

    #[test]
    fn a_bare_name_is_found_in_local_bin_when_path_misses_it() {
        // The actual failure: a systemd user unit's PATH is
        // /usr/local/bin:/usr/bin, and pi lives in ~/.local/bin.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(".local/bin/pi");
        executable(&bin);
        let found = with_env("/usr/local/bin:/usr/bin", dir.path(), || binary(Path::new("pi")));
        assert_eq!(found.unwrap(), bin);
    }

    #[test]
    fn cargo_bin_is_searched_too() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join(".cargo/bin/jj");
        executable(&bin);
        let found = with_env("/nonexistent", dir.path(), || binary(Path::new("jj")));
        assert_eq!(found.unwrap(), bin);
    }

    #[test]
    fn path_wins_over_the_home_locations() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = dir.path().join("usr/bin/pi");
        executable(&on_path);
        executable(&dir.path().join(".local/bin/pi"));
        let found = with_env(&dir.path().join("usr/bin").to_string_lossy(), dir.path(), || {
            binary(Path::new("pi"))
        });
        assert_eq!(found.unwrap(), on_path, "an explicit PATH entry is the operator's choice");
    }

    #[test]
    fn an_explicit_path_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("somewhere/odd/pi");
        executable(&bin);
        let found = with_env("/nonexistent", dir.path(), || binary(&bin));
        assert_eq!(found.unwrap(), bin);
    }

    #[test]
    fn an_explicit_path_that_does_not_exist_says_so_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope/pi");
        let err = with_env("/nonexistent", dir.path(), || binary(&missing)).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("nope/pi"), "the error names the path: {err}");
    }

    #[test]
    fn a_missing_binary_names_every_location_tried() {
        // "not found" without "where I looked" is not actionable — which is
        // exactly how this bug reached a user.
        let dir = tempfile::tempdir().unwrap();
        let err = with_env("/usr/local/bin:/usr/bin", dir.path(), || binary(Path::new("pi")))
            .unwrap_err();
        assert!(err.contains("/usr/local/bin/pi"), "{err}");
        assert!(err.contains("/usr/bin/pi"), "{err}");
        assert!(err.contains(".local/bin/pi"), "{err}");
        assert!(err.contains(".cargo/bin/pi"), "{err}");
        assert!(err.contains("SKIFF_PI_BINARY"), "and how to override it: {err}");
    }
}
