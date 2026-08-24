use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use change::{
    Author, ChangeService, ChangeState, FizzyConfig, Jj, LandingConfig, LandingService, RoundInput,
    Store, TugboatConfig,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Calls {
    deploy: AtomicUsize,
    comment: AtomicUsize,
    jobs: AtomicUsize,
    order: Mutex<Vec<&'static str>>,
}

async fn deploy(State(calls): State<Arc<Calls>>) -> axum::Json<Value> {
    calls.deploy.fetch_add(1, Ordering::SeqCst);
    calls.order.lock().unwrap().push("deploy");
    axum::Json(json!({ "jobs": [{ "name": "skiff", "job_id": "job-1" }] }))
}

async fn job(State(calls): State<Arc<Calls>>, Path(id): Path<String>) -> axum::Json<Value> {
    assert_eq!(id, "job-1");
    calls.jobs.fetch_add(1, Ordering::SeqCst);
    axum::Json(json!({ "id": id, "outcome": { "ok": true, "error": null } }))
}

async fn comment(State(calls): State<Arc<Calls>>) -> (axum::http::StatusCode, axum::Json<Value>) {
    calls.comment.fetch_add(1, Ordering::SeqCst);
    calls.order.lock().unwrap().push("comment");
    (
        axum::http::StatusCode::CREATED,
        axum::Json(json!({
            "id": "comment-1",
            "url": "http://fizzy.test/comment-1",
            "body": { "plain_text": "landed" }
        })),
    )
}

#[tokio::test]
async fn finish_records_every_tail_outcome_and_never_repeats_completed_steps() {
    let root = tempfile::tempdir().unwrap();
    let repos = root.path().join("repos");
    std::fs::create_dir_all(repos.join("demo/.jj")).unwrap();
    let store = Store::new(root.path().join("changes"));
    store.create("demo", 81, Some("tail test"), None).unwrap();
    store
        .add_round(
            "demo",
            81,
            RoundInput {
                author: Author::Agent,
                change_id: "k".repeat(32),
                note: None,
                gates_ran: Vec::new(),
                worth_knowing: Vec::new(),
            },
            |_| Ok(()),
        )
        .unwrap();
    store.transition("demo", 81, ChangeState::InReview).unwrap();
    store.transition("demo", 81, ChangeState::Landing).unwrap();
    store.complete_landing("demo", 81, "abc123").unwrap();

    let calls = Arc::new(Calls::default());
    let app = Router::new()
        .route("/deploy", post(deploy))
        .route("/jobs/{id}", get(job))
        .route("/1/cards/81/comments.json", post(comment))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let token_file = root.path().join("fizzy-token");
    std::fs::write(&token_file, "token\n").unwrap();
    let service = ChangeService::new(store.clone(), repos, Jj::new("jj"));
    let landing = LandingService::new(
        service,
        LandingConfig {
            remote: "origin".to_owned(),
            bookmark: "main".to_owned(),
            push_attempts: 3,
            record: None,
            tugboat: Some(TugboatConfig {
                base: format!("http://{addr}"),
                token: "token".to_owned(),
                poll_interval: Duration::from_millis(1),
                poll_deadline: Duration::from_secs(1),
            }),
            fizzy: Some(FizzyConfig {
                base: format!("http://{addr}"),
                account: "1".to_owned(),
                token_file,
                timeout: Duration::from_secs(1),
            }),
        },
    )
    .unwrap();

    let first = landing.finish("demo", 81).await.unwrap();
    assert!(first.card_comment_attempted);
    assert!(first.deploy_triggered);
    assert_eq!(first.deploy_jobs_finished, 1);
    let change = store.require("demo", 81).unwrap();
    assert!(change.card_comment.unwrap().ok);
    assert!(
        change.deploy.unwrap().services[0]
            .outcome
            .as_ref()
            .unwrap()
            .ok
    );

    let second = landing.finish("demo", 81).await.unwrap();
    assert!(!second.card_comment_attempted);
    assert!(!second.deploy_triggered);
    assert_eq!(second.deploy_jobs_finished, 0);
    assert_eq!(calls.deploy.load(Ordering::SeqCst), 1);
    assert_eq!(calls.comment.load(Ordering::SeqCst), 1);
    assert_eq!(calls.jobs.load(Ordering::SeqCst), 1);
    assert_eq!(*calls.order.lock().unwrap(), ["deploy", "comment"]);
}
