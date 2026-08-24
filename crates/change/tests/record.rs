use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use change::{
    Annotation, AnnotationSide, Author, Change, ChangeState, Commit, Landed, Record, RecordConfig,
    Request, Round,
};

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn record_export_is_private_atomic_and_retryable_without_duplicate_commits() {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin.git");
    git(root.path(), &["init", "--bare", origin.to_str().unwrap()]);
    let checkout = root.path().join("record");
    git(
        root.path(),
        &[
            "clone",
            origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    let record = Record::new(RecordConfig {
        dir: checkout.clone(),
        remote: "origin".to_owned(),
        git_binary: "git".into(),
    })
    .unwrap();
    let change = Change {
        repo: "fleet".to_owned(),
        card: 81,
        title: Some("record test".to_owned()),
        session: Some("pi:private".to_owned()),
        state: ChangeState::Shipped,
        created_at: "created".to_owned(),
        updated_at: "updated".to_owned(),
        rounds: vec![Round {
            n: 1,
            author: Author::Agent,
            change_id: "k".repeat(32),
            note: Some("private round note".to_owned()),
            gates_ran: vec!["cargo test".to_owned()],
            worth_knowing: Vec::new(),
            created_at: "round".to_owned(),
            annotations: vec![Annotation {
                id: "private-id".to_owned(),
                path: "a.txt".to_owned(),
                line: 1,
                side: AnnotationSide::New,
                text: "public reason".to_owned(),
                created_at: "annotation".to_owned(),
            }],
            commit: Some(Commit {
                change_id: "k".repeat(32),
                commit_id: "abc123".to_owned(),
                description: "description".to_owned(),
                author_email: "private@example.invalid".to_owned(),
                timestamp: "commit".to_owned(),
                parents: Vec::new(),
            }),
            divergent: false,
        }],
        last_request: Some(Request {
            note: "private request".to_owned(),
            at: "request".to_owned(),
        }),
        landed: Some(Landed {
            tip: "abc123".to_owned(),
            at: "landed".to_owned(),
        }),
        last_landing: None,
        card_comment: None,
        record_export: None,
        deploy: None,
        path: Some("/private/checkout".to_owned()),
    };
    let diffs = BTreeMap::from([(1, "diff --git a/a.txt b/a.txt\n".to_owned())]);

    record.export(&change, &diffs).unwrap();
    record.export(&change, &diffs).unwrap();

    let count = git(
        root.path(),
        &[
            "--git-dir",
            origin.to_str().unwrap(),
            "rev-list",
            "--count",
            "--all",
        ],
    );
    assert_eq!(count.trim(), "1", "a retry must not create another commit");
    let json = std::fs::read_to_string(checkout.join("fleet/81.json")).unwrap();
    assert!(json.contains("public reason"));
    for private in [
        "pi:private",
        "private round note",
        "private request",
        "private-id",
        "private@example.invalid",
        "/private/checkout",
    ] {
        assert!(!json.contains(private), "record leaked {private}");
    }
    assert!(
        std::fs::read_dir(checkout.join("fleet"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".81.")),
        "temporary files must not survive"
    );
}
