//! The few jj facts dw needs about the repository you are standing in.
//! Reads run --ignore-working-copy (they must not snapshot or contend for
//! the op lock from a status command); `describe` and `new` — ship's two
//! mutations — run without it, exactly like a human's own jj commands, and
//! both land in the operation log where they are undoable.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Repo {
    pub root: PathBuf,
    pub name: String,
}

/// The jj repository containing `dir`, or None — dw's "yours" register
/// simply disappears outside a repo, it never errors.
pub fn containing_repo(dir: &Path) -> Option<Repo> {
    let output = Command::new("jj")
        .args(["--ignore-working-copy", "root"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let name = root.file_name()?.to_string_lossy().to_string();
    Some(Repo { root, name })
}

impl Repo {
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("jj")
            .args(args)
            .current_dir(&self.root)
            .output()
            .context("running jj — is it installed?")?;
        if !output.status.success() {
            bail!("jj {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Paths the working copy currently changes — the "yours" readout.
    pub fn changed_paths(&self) -> Result<Vec<String>> {
        let stdout = self.run(&["--ignore-working-copy", "diff", "-r", "@", "--summary"])?;
        Ok(stdout.lines().map(|line| line.to_string()).collect())
    }

    /// Give the working-copy commit its sentence and return its change id.
    pub fn describe_working_copy(&self, sentence: &str) -> Result<String> {
        self.run(&["describe", "-m", sentence])?;
        let id = self.run(&["--ignore-working-copy", "log", "--no-graph", "-r", "@", "-T", "change_id"])?;
        Ok(id.trim().to_string())
    }

    /// Move @ off the shipped round so the landing rebases a closed commit,
    /// not the commit you are still typing into.
    pub fn new_working_copy(&self) -> Result<()> {
        self.run(&["new"]).map(|_| ())
    }
}
