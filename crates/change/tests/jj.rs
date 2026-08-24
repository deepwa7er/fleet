use std::path::Path;
use std::process::Command;

use change::{AnnotationSide, Author, ChangeService, Jj, RoundInput, Store, is_full_change_id};

fn jj(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "jj {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn change_id(repo: &Path) -> String {
    jj(repo, &["log", "--no-graph", "-r", "@", "-T", "change_id"])
        .trim()
        .to_owned()
}

fn input(id: String) -> RoundInput {
    RoundInput {
        author: Author::Agent,
        change_id: id,
        note: None,
        gates_ran: Vec::new(),
        worth_knowing: Vec::new(),
    }
}

#[test]
fn real_jj_rounds_enrichment_diffs_and_anchor_validation() {
    if Command::new("jj").arg("--version").output().is_err() {
        eprintln!("skipping real jj integration: jj is not installed");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let repos = root.path().join("repos");
    let repo = repos.join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init", "--colocate"]);
    jj(
        &repo,
        &["config", "set", "--repo", "user.name", "Change Test"],
    );
    jj(
        &repo,
        &[
            "config",
            "set",
            "--repo",
            "user.email",
            "change@example.invalid",
        ],
    );

    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    jj(&repo, &["describe", "-m", "round one"]);
    let first = change_id(&repo);
    assert!(is_full_change_id(&first));
    jj(&repo, &["new"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    jj(&repo, &["describe", "-m", "round two"]);
    let second = change_id(&repo);

    let service = ChangeService::new(
        Store::new(root.path().join("changes")),
        &repos,
        Jj::new("jj"),
    );
    service
        .create("demo", 81, Some("two rounds"), None)
        .unwrap();
    service.add_round("demo", 81, input(first.clone())).unwrap();
    service
        .add_round("demo", 81, input(second.clone()))
        .unwrap();

    let change = service.get("demo", 81).unwrap();
    assert_eq!(
        change.rounds[0].commit.as_ref().unwrap().description,
        "round one"
    );
    assert!(
        change.rounds[1]
            .commit
            .as_ref()
            .unwrap()
            .parents
            .contains(&first)
    );
    let round = service.round_diff("demo", 81, 2).unwrap();
    assert!(round.contains_anchor("a.txt", AnnotationSide::New, 2));
    service
        .add_annotation(
            "demo",
            81,
            2,
            "a.txt",
            2,
            AnnotationSide::New,
            "the second round grows the file",
        )
        .unwrap();
    assert!(
        service
            .add_annotation(
                "demo",
                81,
                2,
                "a.txt",
                99,
                AnnotationSide::New,
                "this line does not exist",
            )
            .is_err()
    );
    let cumulative = service.cumulative_diff("demo", 81).unwrap();
    assert!(cumulative.contains_anchor("a.txt", AnnotationSide::New, 1));
    assert!(cumulative.contains_anchor("a.txt", AnnotationSide::New, 2));
}
