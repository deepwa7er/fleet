//! Producing a SCIP index for a project: run `rust-analyzer scip`, read the
//! protobuf it writes, clean up.
//!
//! rust-analyzer does the semantic heavy lifting (roughly a `cargo check` of
//! the workspace, tens of seconds); atlas never parses Rust itself.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use protobuf::Message;
use scip::types::Index;

/// Run `rust-analyzer scip` over `root` and parse the resulting index.
pub fn generate_index(rust_analyzer: &str, root: &Path) -> Result<Index> {
    let output_path = std::env::temp_dir().join(format!(
        "atlas-{}-{}.scip",
        std::process::id(),
        root.file_name().and_then(|n| n.to_str()).unwrap_or("index")
    ));

    let output = Command::new(rust_analyzer)
        .arg("scip")
        .arg(".")
        .arg("--output")
        .arg(&output_path)
        .current_dir(root)
        .output()
        .with_context(|| format!("running {rust_analyzer} scip in {}", root.display()))?;

    let result = (|| {
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{rust_analyzer} scip failed ({}): {}",
                output.status,
                stderr
                    .trim_end()
                    .lines()
                    .rev()
                    .take(10)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        let bytes = std::fs::read(&output_path)
            .with_context(|| format!("reading SCIP output {}", output_path.display()))?;
        Index::parse_from_bytes(&bytes).context("parsing SCIP index")
    })();

    // The index is parsed (or generation failed); the temp file has served
    // its purpose either way.
    let _ = std::fs::remove_file(&output_path);
    result
}

/// The project's current commit, when `root` is a git work tree.
pub fn commit_hash(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!hash.is_empty()).then_some(hash)
}
