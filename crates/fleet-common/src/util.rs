//! Small helpers every service re-implemented: env lookup with a default, the
//! XDG data path for a service's database, epoch seconds, and durable writes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The environment variable `key`, or `default` when unset.
pub fn env_or(key: &str, default: impl Into<String>) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

/// Default database path for a service:
/// `$XDG_DATA_HOME/<service>/<file>` (falling back to `~/.local/share`).
/// On the VPS, services override this via their `<SVC>_DB` env / systemd
/// `StateDirectory`; this default serves local development.
pub fn default_db_path(service: &str, file: &str) -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        });
    base.join(service).join(file)
}

/// Seconds since the Unix epoch (0 if the clock reads before it, which never
/// happens in practice).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Durably replace `path` with `contents`: write a sibling temp file, fsync
/// it, atomically rename it into place, and fsync the directory so the rename
/// itself survives a crash. Readers never observe a partial file.
pub fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_content() {
        let dir = std::env::temp_dir().join(format!("fleet-common-aw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

}
