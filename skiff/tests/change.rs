use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use change::{
    AnnotationSide, Author, ChangeService, ChangeState, Jj, LandingConfig, RoundInput,
    Store as ChangeStore,
};
use futures_util::{SinkExt, StreamExt};
use skiff::ingest::Topic;
use skiff::run::Runs;
use skiff::run::muse_exec::MuseConfig;
use skiff::server::{AppState, router};
use skiff::store::Store;
use skiff::views::{ViewData, ViewSpec};
use skiff::wire::{ClientFrame, Command, ServerFrame};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const DEADLINE: Duration = Duration::from_secs(5);

fn run(program: &str, cwd: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new(program)
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

struct Harness {
    _root: tempfile::TempDir,
    addr: SocketAddr,
    origin: std::path::PathBuf,
    changes: ChangeService,
}

async fn start() -> Harness {
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
        &["config", "set", "--repo", "user.name", "Skiff Test"],
    );
    jj(
        &repo,
        &[
            "config",
            "set",
            "--repo",
            "user.email",
            "skiff@example.invalid",
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
    std::fs::write(repo.join("a.txt"), "base\nreview me\n").unwrap();
    jj(&repo, &["describe", "-m", "review round"]);
    let change_id = jj(&repo, &["log", "--no-graph", "-r", "@", "-T", "change_id"])
        .trim()
        .to_owned();
    jj(&repo, &["new"]);

    let changes = ChangeService::new(
        ChangeStore::new(root.path().join("changes")),
        &repos,
        Jj::new("jj"),
    );
    changes
        .create("demo", 81, Some("review me"), Some("pi:abc"))
        .unwrap();
    changes
        .add_round(
            "demo",
            81,
            RoundInput {
                author: Author::Agent,
                change_id,
                note: None,
                gates_ran: vec!["cargo test".to_owned()],
                worth_knowing: Vec::new(),
            },
        )
        .unwrap();
    changes
        .transition("demo", 81, ChangeState::InReview)
        .unwrap();

    let store = Store::in_memory().unwrap();
    let (topics, _) = tokio::sync::broadcast::channel::<Topic>(64);
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("abc.jsonl"),
        "{\"type\":\"session\",\"cwd\":\"/tmp\",\"timestamp\":\"2026-08-23T10:00:00.000Z\"}\n",
    )
    .unwrap();
    let fake_pi = root.path().join("fake-pi");
    std::fs::write(
        &fake_pi,
        r##"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    command = json.loads(line)
    print(json.dumps({"type": "response", "id": command["id"], "success": True}), flush=True)
"##,
    )
    .unwrap();
    std::fs::set_permissions(&fake_pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runs = Runs::new(
        store.clone(),
        topics.clone(),
        fake_pi,
        sessions,
        true,
        MuseConfig {
            binary: root.path().join("missing-muse"),
            session_dir: root.path().join("muse-sessions"),
            session_dir_explicit: true,
        },
        "http://127.0.0.1:1",
    );
    let state = AppState::with_changes(
        store,
        runs,
        topics,
        changes.clone(),
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
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), "<!doctype html>").unwrap();
    let app = router(state, dist.path().to_owned());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dist = dist;
        axum::serve(listener, app).await.unwrap();
    });
    Harness {
        _root: root,
        addr,
        origin,
        changes,
    }
}

#[tokio::test]
async fn request_changes_reaches_the_bound_session_before_reopening_review() {
    if ProcessCommand::new("jj").arg("--version").output().is_err() {
        eprintln!("skipping change socket integration: jj is not installed");
        return;
    }
    let harness = start().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", harness.addr))
        .await
        .unwrap();
    assert!(matches!(next(&mut socket).await, ServerFrame::Hello { .. }));
    send(
        &mut socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Change {
                repo: "demo".to_owned(),
                card: 81,
                round: Some(1),
            },
        },
    )
    .await;
    assert_eq!(
        change_snapshot(&mut socket).await.state,
        ChangeState::InReview
    );
    send(
        &mut socket,
        ClientFrame::Command {
            req: 3,
            cmd: Command::RequestChanges {
                repo: "demo".to_owned(),
                card: 81,
                note: "tighten the explanation".to_owned(),
            },
        },
    )
    .await;
    assert!(matches!(
        next(&mut socket).await,
        ServerFrame::Ack { req: 3 }
    ));
    let reopened = change_snapshot(&mut socket).await;
    assert_eq!(reopened.state, ChangeState::Working);
    assert_eq!(
        reopened.last_request.unwrap().note,
        "tighten the explanation"
    );
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next(socket: &mut Socket) -> ServerFrame {
    loop {
        let message = tokio::time::timeout(DEADLINE, socket.next())
            .await
            .expect("timed out")
            .expect("socket closed")
            .expect("socket error");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn send(socket: &mut Socket, frame: ClientFrame) {
    socket
        .send(Message::text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();
}

async fn change_snapshot(socket: &mut Socket) -> change::Change {
    loop {
        if let ServerFrame::Snapshot {
            data: ViewData::Change(view),
            ..
        } = next(socket).await
        {
            return view.change;
        }
    }
}

#[tokio::test]
async fn review_annotation_and_approval_flow_through_one_live_socket() {
    if ProcessCommand::new("jj").arg("--version").output().is_err() {
        eprintln!("skipping change socket integration: jj is not installed");
        return;
    }
    let harness = start().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", harness.addr))
        .await
        .unwrap();
    assert!(matches!(next(&mut socket).await, ServerFrame::Hello { .. }));
    send(
        &mut socket,
        ClientFrame::Subscribe {
            sub: 1,
            view: ViewSpec::Change {
                repo: "demo".to_owned(),
                card: 81,
                round: Some(1),
            },
        },
    )
    .await;
    assert_eq!(
        change_snapshot(&mut socket).await.state,
        ChangeState::InReview
    );

    send(
        &mut socket,
        ClientFrame::Command {
            req: 1,
            cmd: Command::AnnotateChange {
                repo: "demo".to_owned(),
                card: 81,
                round: 1,
                path: "a.txt".to_owned(),
                line: 2,
                side: AnnotationSide::New,
                text: "the exact reviewed line".to_owned(),
            },
        },
    )
    .await;
    assert!(matches!(
        next(&mut socket).await,
        ServerFrame::Ack { req: 1 }
    ));
    let annotated = change_snapshot(&mut socket).await;
    assert_eq!(
        annotated.rounds[0].annotations[0].text,
        "the exact reviewed line"
    );

    send(
        &mut socket,
        ClientFrame::Command {
            req: 2,
            cmd: Command::ApproveChange {
                repo: "demo".to_owned(),
                card: 81,
            },
        },
    )
    .await;
    assert!(matches!(
        next(&mut socket).await,
        ServerFrame::Ack { req: 2 }
    ));
    loop {
        if change_snapshot(&mut socket).await.state == ChangeState::Shipped {
            break;
        }
    }
    let shipped = harness.changes.store().require("demo", 81).unwrap();
    let origin_tip = run(
        "git",
        harness._root.path(),
        &[
            "--git-dir",
            harness.origin.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ],
    );
    assert_eq!(shipped.landed.unwrap().tip, origin_tip.trim());
}
