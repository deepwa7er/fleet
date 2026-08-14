//! The fleet IDE. Milestone 1: the shell — project tree, tabs, editor pane,
//! status bar — in the DW-001 palette. Roadmap in README.md.
//!
//! Usage: `cargo run -- [path]` — a directory opens as the workspace root
//! (default: the current directory); a file opens its parent directory as the
//! root with that file already open.

mod app;
mod lsp;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeRegistry};
use gpui_component_assets::Assets;

use crate::app::IdeShell;
use crate::workspace::{LocalWorkspace, WorkspaceService};

fn main() {
    let arg = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cannot determine current directory"));
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

    let workspace: Arc<dyn WorkspaceService> = match LocalWorkspace::new(&root) {
        Ok(workspace) => Arc::new(workspace),
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

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            app::init(cx);
            apply_deepwater_theme(cx);
            cx.activate(true);

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
