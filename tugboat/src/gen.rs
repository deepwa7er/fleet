//! `tugboat fleet gen` — derive the fleet's registry configs from each
//! service's `deploy.toml`, so "the list of services" is declared once and
//! everything downstream is generated.
//!
//! Two artifacts today:
//!
//! 1. **breakwater's route table** — every in-monorepo service declaring a
//!    `port` gets a `[[routes]]` entry, written between marker comments in
//!    `breakwater/breakwater.toml`. Routes outside the markers (sonar's
//!    units, `docs`' serve_dir, `source`'s dev-box upstream, external members
//!    like lagoon whose manifests aren't in this repo) stay hand-written.
//!
//! 2. **the backup set** — `fleet-backup/state.sh`, listing the SQLite
//!    databases to snapshot and the directories to back up in place, from
//!    each manifest's `[state]` block plus `fleet.toml`'s `[backup]` extras.
//!    The backup script refuses to run without it, so the set can never
//!    silently regress to "whatever the script hardcodes".
//!
//! `--check` rewrites nothing: it fails (with a diff summary) when a file
//! doesn't match what the declarations produce. It is a pre-PR gate in
//! `.agents/skills/fleet/SKILL.md` §3 — CI was archived 2026-08-13
//! (`docs/archive/ci.ARCHIVED.md`), so nothing runs it automatically. Skip it after adding a
//! service and the registries drift silently until deploy time.
//!
//! Only in-monorepo deployables feed generation, deliberately: generation must
//! produce identical output from a checkout of this repository alone and from
//! the dev box.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::fleet::{discover_deployables, Fleet};
use crate::manifest;

const ROUTES_BEGIN: &str = "# ── BEGIN GENERATED ROUTES (tugboat fleet gen) ──";
const ROUTES_END: &str = "# ── END GENERATED ROUTES ──";

/// Generate (or with `check`, verify) every derived registry. Returns an error
/// listing the stale files in check mode.
pub fn run(fleet: &Fleet, check: bool) -> Result<()> {
    let root = fleet.root_dir();
    let services = load_declarations(&root)?;

    let mut stale = Vec::new();
    apply(
        &root.join("breakwater/breakwater.toml"),
        &render_routes_file(&root, &services)?,
        check,
        &mut stale,
    )?;
    apply(
        &root.join("fleet-backup/state.sh"),
        &render_state_sh(fleet, &services),
        check,
        &mut stale,
    )?;

    if check {
        if stale.is_empty() {
            println!("✓ fleet gen --check: all generated registries match their declarations");
            Ok(())
        } else {
            bail!(
                "generated registries are stale: {} — run `tugboat fleet gen` and commit",
                stale.join(", ")
            );
        }
    } else {
        println!("✓ fleet gen: registries written (breakwater routes, backup state)");
        Ok(())
    }
}

/// One service's registry-relevant declarations.
struct Declared {
    name: String,
    port: Option<u16>,
    state_db: Option<String>,
    state_files: bool,
}

/// Read every in-monorepo deployable's manifest. Sorted by name, so the
/// generated output is stable.
fn load_declarations(root: &Path) -> Result<Vec<Declared>> {
    let mut out = Vec::new();
    for d in discover_deployables(root)? {
        let m = manifest::parse(&d.manifest_path)
            .with_context(|| format!("parsing {}", d.manifest_path.display()))?;
        let (state_db, state_files) = match &m.state {
            Some(s) => (s.db.clone(), s.files),
            None => (None, false),
        };
        out.push(Declared { name: m.name, port: m.port, state_db, state_files });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The full new content of breakwater.toml: everything outside the marker
/// block preserved verbatim, the block re-rendered from declarations.
fn render_routes_file(root: &Path, services: &[Declared]) -> Result<String> {
    let path = root.join("breakwater/breakwater.toml");
    let current = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    let begin = current.find(ROUTES_BEGIN).with_context(|| {
        format!("{} has no `{ROUTES_BEGIN}` marker — add the marker pair once", path.display())
    })?;
    let end_marker = current[begin..]
        .find(ROUTES_END)
        .map(|i| begin + i)
        .with_context(|| format!("{} has a BEGIN marker but no END marker", path.display()))?;
    let end = current[end_marker..]
        .find('\n')
        .map(|i| end_marker + i + 1)
        .unwrap_or(current.len());

    let mut block = String::new();
    let _ = writeln!(block, "{ROUTES_BEGIN}");
    let _ = writeln!(
        block,
        "# One route per fleet service declaring `port` in its deploy.toml — derived\n\
         # by `tugboat fleet gen`; `--check` verifies. Don't edit inside the\n\
         # markers: change the service's deploy.toml and re-run the generator.\n\
         # Hand-written routes (sonar's units, docs, source, external members)\n\
         # live outside this block."
    );
    for s in services.iter().filter(|s| s.port.is_some()) {
        let port = s.port.expect("filtered on is_some");
        let _ = writeln!(block, "\n[[routes]]\nlabel = \"{}\"\nupstream = \"127.0.0.1:{port}\"", s.name);
    }
    let _ = writeln!(block, "\n{ROUTES_END}");
    // Everything after the END marker's line is preserved verbatim.
    Ok(format!("{}{}{}", &current[..begin], block, &current[end..]))
}

/// The backup set, as a sh fragment the fleet-backup script sources.
fn render_state_sh(fleet: &Fleet, services: &[Declared]) -> String {
    let mut dbs: Vec<String> = services
        .iter()
        .filter_map(|s| s.state_db.as_ref().map(|db| format!("{}/{db}", s.name)))
        .collect();
    dbs.extend(fleet.backup.sqlite.iter().cloned());
    dbs.sort();

    let mut dirs: Vec<String> =
        services.iter().filter(|s| s.state_files).map(|s| s.name.clone()).collect();
    dirs.extend(fleet.backup.dirs.iter().cloned());
    dirs.sort();

    format!(
        "# GENERATED by `tugboat fleet gen` — do not edit.\n\
         # Sources: each service's deploy.toml [state] block + fleet.toml [backup].\n\
         # Adding a stateful service means declaring it there, not editing scripts.\n\
         \n\
         # /var/lib/<entry> SQLite databases, snapshotted via the online-backup API.\n\
         SQLITE_DBS=\"{}\"\n\
         \n\
         # /var/lib/<entry> directories whose plain files are backed up in place.\n\
         RAW_DIRS=\"{}\"\n",
        dbs.join(" "),
        dirs.join(" ")
    )
}

/// Write `content` to `path`, or in check mode compare and record staleness.
fn apply(path: &Path, content: &str, check: bool, stale: &mut Vec<String>) -> Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == content {
        return Ok(());
    }
    if check {
        stale.push(path.display().to_string());
    } else {
        std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
        println!("  wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, port: Option<u16>, db: Option<&str>, files: bool) -> Declared {
        Declared {
            name: name.into(),
            port,
            state_db: db.map(Into::into),
            state_files: files,
        }
    }

    /// The route block is regenerated between markers; hand-written routes
    /// before and after survive byte-for-byte.
    #[test]
    fn routes_block_replaces_only_marked_region() {
        let tmp = std::env::temp_dir().join(format!("tugboat-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("breakwater")).unwrap();
        let existing = format!(
            "base_domain = \"x.example\"\n\n[[routes]]\nlabel = \"music\"\nupstream = \"127.0.0.1:4533\"\n\n{ROUTES_BEGIN}\nold\n{ROUTES_END}\n\n[[routes]]\nlabel = \"source\"\nupstream = \"100.0.0.1:7879\"\n"
        );
        std::fs::write(tmp.join("breakwater/breakwater.toml"), &existing).unwrap();

        let services = vec![decl("drydock", Some(8093), None, false), decl("tide", Some(8094), None, false)];
        let rendered = render_routes_file(&tmp, &services).unwrap();

        assert!(rendered.starts_with("base_domain = \"x.example\""));
        assert!(rendered.contains("label = \"music\""), "hand-written route before block survives");
        assert!(rendered.contains("label = \"source\""), "hand-written route after block survives");
        assert!(rendered.contains("label = \"drydock\"\nupstream = \"127.0.0.1:8093\""));
        assert!(rendered.contains("label = \"tide\"\nupstream = \"127.0.0.1:8094\""));
        assert!(!rendered.contains("\nold\n"), "stale block content replaced");

        // Idempotent: rendering the rendered file again is a fixed point.
        std::fs::write(tmp.join("breakwater/breakwater.toml"), &rendered).unwrap();
        assert_eq!(render_routes_file(&tmp, &services).unwrap(), rendered);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The backup set merges manifest [state] declarations with fleet.toml
    /// extras, sorted for stable output.
    #[test]
    fn state_sh_merges_declarations_and_extras() {
        let fleet: Fleet = toml::from_str(
            "root = \"/x\"\n[backup]\nsqlite = [\"lagoon/lagoon.sqlite\"]\ndirs = [\"tugboat\"]\n",
        )
        .unwrap();
        let services = vec![
            decl("drydock", Some(8093), Some("drydock.db"), false),
            decl("ferry", Some(7777), None, true),
            decl("breakwater", None, None, true),
        ];
        let sh = render_state_sh(&fleet, &services);
        assert!(sh.contains("SQLITE_DBS=\"drydock/drydock.db lagoon/lagoon.sqlite\""));
        assert!(sh.contains("RAW_DIRS=\"breakwater ferry tugboat\""));
    }
}
