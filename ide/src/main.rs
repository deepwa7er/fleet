//! The fleet IDE. Milestone 1: the shell — project tree, tabs, editor pane,
//! status bar — in the DW-001 palette. Roadmap in README.md.
//!
//! Usage: `cargo run -- [path]` — a directory opens as the workspace root
//! (default: the current directory); a file opens its parent directory as the
//! root with that file already open.

mod app;
mod providers;
mod search;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeRegistry};
use gpui_component_assets::Assets;
use ide::remote::RemoteWorkspace;
use ide::workspace::{LocalWorkspace, WorkspaceService};

use crate::app::IdeShell;

fn main() {
    let arg = std::env::args().nth(1);

    let (workspace, initial_file, title): (Arc<dyn WorkspaceService>, Option<PathBuf>, String) =
        if let Some((host, remote_path)) = arg.as_deref().and_then(parse_remote) {
            // Remote mode (docs/remote.md §3): the workspace lives where the
            // ssh alias points; the connection is the session.
            let ssh_args = [
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                host.to_string(),
                "ide-server".to_string(),
                "--stdio".to_string(),
            ];
            let connect = RemoteWorkspace::connect("ssh", &ssh_args, remote_path);
            match futures::executor::block_on(connect) {
                Ok(workspace) => {
                    let title = format!(
                        "{}:{} — ide",
                        host,
                        workspace
                            .root()
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(remote_path)
                    );
                    (Arc::new(workspace), None, title)
                }
                Err(err) => {
                    eprintln!(
                        "ide: cannot connect to {host}: {err:#}\n\
                         (a timeout usually means the host is powered off — check `tailscale status`)"
                    );
                    std::process::exit(1);
                }
            }
        } else {
            let arg = arg.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let (root, initial_file) = if arg.is_file() {
                let file = arg.canonicalize().unwrap_or(arg);
                let parent = file
                    .parent()
                    .map(PathBuf::from)
                    .expect("a canonical file path always has a parent");
                (parent, Some(file))
            } else {
                (arg, None)
            };
            let workspace = match LocalWorkspace::new(&root) {
                Ok(workspace) => workspace,
                Err(err) => {
                    eprintln!("ide: {err:#}");
                    std::process::exit(1);
                }
            };
            let title = format!(
                "{} — ide",
                workspace
                    .root()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("ide")
            );
            (Arc::new(workspace), initial_file, title)
        };

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            app::init(cx);
            apply_deepwater_theme(cx);
            cx.activate(true);

            // Auto-save's last line of defense: flush every dirty document
            // before the process exits (docs/remote.md §6).
            cx.on_app_quit({
                let workspace = workspace.clone();
                move |_| {
                    let flush = workspace.flush_all();
                    async move {
                        if let Err(err) = flush.await {
                            eprintln!("ide: flush on quit failed: {err:#}");
                        }
                    }
                }
            })
            .detach();

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1440.), px(900.)),
                    cx,
                ))),
                ..Default::default()
            };

            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
                    let shell = cx.new(|cx| {
                        IdeShell::new(workspace.clone(), initial_file.clone(), window, cx)
                    });
                    cx.new(|cx| Root::new(shell, window, cx).bg(cx.theme().background))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}

/// scp-style remote target: `host:path` where `host` has no `/` and the
/// whole argument is not an existing local path (`./a:b` stays local).
fn parse_remote(arg: &str) -> Option<(&str, &str)> {
    let (host, path) = arg.split_once(':')?;
    if host.is_empty() || host.contains('/') || std::path::Path::new(arg).exists() {
        return None;
    }
    Some((host, path))
}

/// Load the embedded DW-001 theme (themes/deepwater.json) and make it active.
/// Light is the default; following the system appearance is a follow-up.
fn apply_deepwater_theme(cx: &mut App) {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(include_str!("../themes/deepwater.json"))
        .expect("embedded deepwater.json must parse");

    let config = ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from("Deepwater Light"))
        .cloned()
        .expect("embedded theme file must define Deepwater Light");
    let mode = config.mode;
    Theme::global_mut(cx).apply_config(&config);
    Theme::change(mode, None, cx);
}
