//! The client's types are generated from the server's, and drift is a failing
//! test (DW-004 §10).
//!
//! This is the point at which "typed end to end" stops being an aspiration: a
//! protocol change the client has not absorbed fails here rather than at
//! runtime, in a browser, on a phone.
//!
//! ts-rs can write these files itself on every `cargo test`, but that makes
//! drift invisible — the files simply change under you. Generating into a
//! temporary directory and comparing makes the gate real. To accept a change:
//!
//! ```sh
//! SKIFF_WRITE_TYPES=1 cargo test -p skiff --test types
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use skiff::wire::{ClientFrame, ServerFrame};
use ts_rs::TS;

/// The types declare `#[ts(export_to = "gen/")]`, and ts-rs joins that onto
/// whatever base directory it is given — so the base is `web/src`, and the
/// files land in `web/src/gen`. Keeping the declared path free of `..` is what
/// lets the same call target a temporary directory for the drift comparison.
const GEN_SUBDIR: &str = "gen";

fn checked_in_base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src")
}

/// Generate the whole reachable type graph under `base`.
///
/// Both frame types are roots because the protocol is bidirectional; between
/// them they reach every view, every view's data, and the domain model.
fn generate_into(base: &Path) {
    std::fs::create_dir_all(base.join(GEN_SUBDIR)).expect("creating the output directory");
    ClientFrame::export_all_to(base).expect("exporting the client frame graph");
    ServerFrame::export_all_to(base).expect("exporting the server frame graph");
}

fn read_dir(dir: &Path) -> BTreeSet<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "ts"))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let body = std::fs::read_to_string(e.path()).expect("reading a generated file");
            (name, body)
        })
        .collect()
}

#[test]
fn the_checked_in_client_types_match_the_server() {
    let out = tempfile::tempdir().expect("a temporary directory");
    generate_into(out.path());
    let generated = read_dir(&out.path().join(GEN_SUBDIR));
    assert!(!generated.is_empty(), "the export produced nothing; the graph roots are wrong");

    if std::env::var_os("SKIFF_WRITE_TYPES").is_some() {
        let target = checked_in_base().join(GEN_SUBDIR);
        // Remove first: a type that no longer exists must not linger as a
        // stale file the client can still import.
        for (name, _) in read_dir(&target) {
            std::fs::remove_file(target.join(name)).expect("removing a stale type");
        }
        std::fs::create_dir_all(&target).expect("creating web/src/gen");
        for (name, body) in &generated {
            std::fs::write(target.join(name), body).expect("writing a type");
        }
        return;
    }

    let checked_in = read_dir(&checked_in_base().join(GEN_SUBDIR));

    let generated_names: BTreeSet<_> = generated.iter().map(|(n, _)| n.as_str()).collect();
    let checked_in_names: BTreeSet<_> = checked_in.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        generated_names, checked_in_names,
        "web/src/gen has the wrong set of types.\n\
         Regenerate with: SKIFF_WRITE_TYPES=1 cargo test -p skiff --test types"
    );

    for ((name, want), (_, got)) in generated.iter().zip(checked_in.iter()) {
        assert_eq!(
            want, got,
            "web/src/gen/{name} has drifted from the Rust type.\n\
             Regenerate with: SKIFF_WRITE_TYPES=1 cargo test -p skiff --test types"
        );
    }
}
