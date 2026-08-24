use std::io::Write;
use std::sync::{Arc, Barrier};

use change::{AnnotationSide, Author, ChangeState, Error, RoundInput, Store};

fn round(change_id: String) -> RoundInput {
    RoundInput {
        author: Author::Agent,
        change_id,
        note: None,
        gates_ran: vec!["cargo test".to_owned()],
        worth_knowing: Vec::new(),
    }
}

#[test]
fn authored_log_replays_and_skips_torn_or_future_lines() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    let created = store
        .create("fleet", 81, Some("model picker"), Some("pi:abc"))
        .unwrap();
    assert_eq!(created.state, ChangeState::Working);
    assert!(matches!(
        store.create("fleet", 81, None, None),
        Err(Error::Exists { .. })
    ));

    store
        .add_round("fleet", 81, round("k".repeat(32)), |_| Ok(()))
        .unwrap();
    store
        .add_annotation(
            "fleet",
            81,
            1,
            "src/main.rs",
            12,
            AnnotationSide::New,
            "the cache is bounded by the source lifetime",
            |_, _| Ok(()),
        )
        .unwrap();
    store
        .transition("fleet", 81, ChangeState::InReview)
        .unwrap();
    let reopened = store
        .request_changes("fleet", 81, "make the timeout explicit")
        .unwrap();
    assert_eq!(reopened.state, ChangeState::Working);

    let path = root.path().join("fleet/81.jsonl");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{{\"event\":\"from_the_future\",\"at\":\"2030-01-01T00:00:00Z\"}}"
    )
    .unwrap();
    write!(file, "{{\"event\":\"round\",\"changeId\":").unwrap();
    file.sync_all().unwrap();

    let replayed = store.require("fleet", 81).unwrap();
    assert_eq!(replayed.rounds.len(), 1);
    assert_eq!(replayed.rounds[0].annotations.len(), 1);
    assert_eq!(
        replayed.last_request.unwrap().note,
        "make the timeout explicit"
    );
}

#[test]
fn state_machine_freezes_authored_review_after_landing_begins() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    store.create("fleet", 82, None, None).unwrap();
    assert!(
        store
            .transition("fleet", 82, ChangeState::InReview)
            .is_err()
    );
    store
        .add_round("fleet", 82, round("k".repeat(32)), |_| Ok(()))
        .unwrap();
    store
        .transition("fleet", 82, ChangeState::InReview)
        .unwrap();
    store.transition("fleet", 82, ChangeState::Landing).unwrap();
    assert!(matches!(
        store.add_round("fleet", 82, round("l".repeat(32)), |_| Ok(())),
        Err(Error::Frozen { .. })
    ));
    let failed = store
        .fail_landing("fleet", 82, "the rebase conflicts", &["k".repeat(32)])
        .unwrap();
    assert_eq!(failed.state, ChangeState::InReview);
    store.transition("fleet", 82, ChangeState::Landing).unwrap();
    let shipped = store.complete_landing("fleet", 82, "abc123").unwrap();
    assert_eq!(shipped.state, ChangeState::Shipped);
    assert_eq!(shipped.landed.unwrap().tip, "abc123");
}

#[test]
fn process_level_file_lock_serializes_independent_writers() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    store.create("fleet", 83, None, None).unwrap();
    let barrier = Arc::new(Barrier::new(9));
    std::thread::scope(|scope| {
        for index in 0..8 {
            let root = root.path().to_owned();
            let barrier = barrier.clone();
            scope.spawn(move || {
                let store = Store::new(root);
                barrier.wait();
                let id = char::from(b'k' + index).to_string().repeat(32);
                store.add_round("fleet", 83, round(id), |_| Ok(())).unwrap();
            });
        }
        barrier.wait();
    });
    let change = store.require("fleet", 83).unwrap();
    assert_eq!(change.rounds.len(), 8);
    assert_eq!(
        change
            .rounds
            .iter()
            .map(|round| round.n)
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>()
    );
}

#[test]
fn list_is_newest_first_and_binding_returns_a_small_reference() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path());
    store
        .create("fleet", 84, Some("first"), Some("pi:one"))
        .unwrap();
    store
        .create("fleet", 85, Some("second"), Some("pi:two"))
        .unwrap();
    store.set_session("fleet", 84, "pi:latest").unwrap();
    let listed = store.list().unwrap();
    assert_eq!(listed[0].card, 84);
    let bound = store.bound_to("pi:latest").unwrap().unwrap();
    assert_eq!(bound.card, 84);
    assert_eq!(bound.rounds, 0);
}
