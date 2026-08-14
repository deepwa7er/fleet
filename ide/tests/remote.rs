//! End-to-end transport test (docs/remote.md §8): spawn the real ide-server
//! binary and drive a RemoteWorkspace through its stdio — the full RPC path
//! with no ssh and no display. Uses non-code files so the server's language
//! hub stays quiet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use futures::executor::block_on;
use ide::remote::RemoteWorkspace;
use ide::workspace::WorkspaceService;

fn connect(root: &Path) -> RemoteWorkspace {
    block_on(RemoteWorkspace::connect(
        env!("CARGO_BIN_EXE_ide-server"),
        &["--stdio".to_string()],
        root.to_str().unwrap(),
    ))
    .expect("connect to ide-server")
}

#[test]
fn remote_workspace_serves_fs_and_search() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/log.txt"), "ballast shifted\n").unwrap();
    std::fs::write(dir.path().join("notes.md"), "# harbor\n").unwrap();

    let ws = connect(dir.path());
    assert_eq!(ws.root(), dir.path().canonicalize().unwrap());

    let entries = block_on(ws.read_dir(ws.root())).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["src", "notes.md"], "dirs first, then files");

    let text = block_on(ws.read_file(&ws.root().join("notes.md"))).unwrap();
    assert_eq!(text, "# harbor\n");

    let files = block_on(ws.list_files()).unwrap();
    assert_eq!(files, [PathBuf::from("notes.md"), PathBuf::from("src/log.txt")]);

    let hits = block_on(ws.search_text("ballast".into(), 10)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, PathBuf::from("src/log.txt"));
    assert_eq!(hits[0].line, 1);
}

#[test]
fn remote_documents_auto_save_server_side() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("log.txt");
    std::fs::write(&file, "old").unwrap();

    let ws = connect(dir.path());
    let file = ws.root().join("log.txt");
    let mut sync = ws.subscribe_sync_state().expect("sync stream");
    assert!(ws.subscribe_sync_state().is_none(), "single subscriber");

    // Edit → the server's own auto-save debounce persists it.
    ws.document_open(&file, "old".into());
    ws.document_changed(&file, "edited over the wire".into());
    assert_eq!(block_on(sync.next()), Some(false));
    assert_eq!(block_on(sync.next()), Some(true), "server flushed");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "edited over the wire"
    );

    // Explicit save is immediate and reports the result.
    block_on(ws.document_save(&file, "saved explicitly".into())).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "saved explicitly");

    // Close flushes whatever the debounce hadn't reached yet.
    ws.document_changed(&file, "final words".into());
    ws.document_closed(&file);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&file).unwrap() == "final words" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "close never flushed");
        block_on(smol::Timer::after(Duration::from_millis(50)));
    }
}

#[test]
fn language_requests_answer_empty_until_5d() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("log.txt"), "x").unwrap();
    let ws = connect(dir.path());
    let file = ws.root().join("log.txt");

    let hover = block_on(ws.hover(&file, lsp_types::Position::new(0, 0))).unwrap();
    assert!(hover.is_none());
    let defs = block_on(ws.definition(&file, lsp_types::Position::new(0, 0))).unwrap();
    assert!(defs.is_empty());
    assert!(ws.completion_triggers(&file).is_empty());
    assert!(ws.subscribe_diagnostics().is_none());
}
