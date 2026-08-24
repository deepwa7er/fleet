use std::path::Path;
use std::process::Command;

use change::{
    Author, ChangeService, ChangeState, Jj, LandingConfig, LandingService, RoundInput, Store,
};

fn run(program: &str, cwd: &Path, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn jj(repo: &Path, args: &[&str]) -> String {
    run("jj", repo, args)
}

fn change_id(repo: &Path, revision: &str) -> String {
    jj(
        repo,
        &["log", "--no-graph", "-r", revision, "-T", "change_id"],
    )
    .trim()
    .to_owned()
}

#[tokio::test]
async fn landing_rebases_and_pushes_the_reviewed_round_to_origin_main() {
    if Command::new("jj").arg("--version").output().is_err() {
        eprintln!("skipping real landing integration: jj is not installed");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin.git");
    run(
        "git",
        root.path(),
        &["init", "--bare", origin.to_str().unwrap()],
    );
    let repos = root.path().join("repos");
    let repo = repos.join("demo");
    std::fs::create_dir_all(&repo).unwrap();
    jj(&repo, &["git", "init", "--colocate"]);
    jj(
        &repo,
        &["config", "set", "--repo", "user.name", "Landing Test"],
    );
    jj(
        &repo,
        &[
            "config",
            "set",
            "--repo",
            "user.email",
            "landing@example.invalid",
        ],
    );
    run(
        "git",
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    std::fs::write(repo.join("a.txt"), "base\n").unwrap();
    jj(&repo, &["describe", "-m", "base"]);
    jj(&repo, &["bookmark", "create", "main", "-r", "@"]);
    jj(
        &repo,
        &["git", "push", "--remote", "origin", "--bookmark", "main"],
    );

    jj(&repo, &["new", "main"]);
    std::fs::write(repo.join("a.txt"), "base\nfeature\n").unwrap();
    jj(&repo, &["describe", "-m", "feature"]);
    let round = change_id(&repo, "@");
    jj(&repo, &["new"]);

    let service = ChangeService::new(
        Store::new(root.path().join("changes")),
        &repos,
        Jj::new("jj"),
    );
    service.create("demo", 81, Some("feature"), None).unwrap();
    service
        .add_round(
            "demo",
            81,
            RoundInput {
                author: Author::Agent,
                change_id: round,
                note: None,
                gates_ran: vec!["cargo test".to_owned()],
                worth_knowing: Vec::new(),
            },
        )
        .unwrap();
    service
        .transition("demo", 81, ChangeState::InReview)
        .unwrap();
    let landing = LandingService::new(
        service.clone(),
        LandingConfig {
            remote: "origin".to_owned(),
            bookmark: "main".to_owned(),
            push_attempts: 3,
            record: None,
            tugboat: None,
            fizzy: None,
        },
    )
    .unwrap();
    landing.begin("demo", 81).unwrap();
    let report = landing.land("demo", 81).await.unwrap();
    assert_eq!(report, Default::default());

    let shipped = service.store().require("demo", 81).unwrap();
    assert_eq!(shipped.state, ChangeState::Shipped);
    let origin_tip = run(
        "git",
        root.path(),
        &[
            "--git-dir",
            origin.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ],
    );
    assert_eq!(shipped.landed.unwrap().tip, origin_tip.trim());
}
