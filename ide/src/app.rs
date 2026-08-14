//! The shell: IntelliJ New UI layout — project tool window, tab strip, editor
//! pane, status bar — in the DW-001 palette (see themes/deepwater.json).
//! DW-001 note: the fleet style guide bans separator borders (rule 1), but the
//! IDE carries a standing exception — 1px `theme.border` lines separate panes,
//! because whitespace alone cannot hold this density. Everything else follows
//! the guide: paper chrome, the editor as a `--fill` recess (rule 2), accent
//! only on interactive/selection states (rule 4), instrumentation voice for
//! the tool-window header and status bar (rule 5).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, WindowExt as _, h_flex,
    highlighter::Language,
    input::{Input, InputBaseState, InputEvent, TabSize},
    list::ListItem,
    resizable::{h_resizable, resizable_panel},
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    tree::{TreeItem, TreeState, tree},
    v_flex,
};

use crate::workspace::WorkspaceService;

actions!(ide, [Save, CloseTab]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-s", Save, Some("IdeShell")),
        // IntelliJ's close-tab chord; ctrl-w belongs to the editor's own map.
        KeyBinding::new("ctrl-f4", CloseTab, Some("IdeShell")),
    ]);
}

/// Directories the project tree never descends into. The tree component loads
/// eagerly (no lazy expansion hook), so build artifacts must be pruned or the
/// initial scan walks gigabytes. Revisit when the tree grows lazy loading.
const PRUNED_DIRS: &[&str] = &[".git", ".worktrees", "target", "node_modules", "tmp"];

struct OpenFile {
    path: PathBuf,
    /// Path relative to the workspace root, for the status bar.
    rel: SharedString,
    /// Tab label: the file name alone.
    name: SharedString,
    language: Language,
    editor: Entity<InputBaseState>,
    dirty: bool,
    _subscriptions: Vec<Subscription>,
}

pub struct IdeShell {
    workspace: Arc<dyn WorkspaceService>,
    tree_state: Entity<TreeState>,
    open: Vec<OpenFile>,
    active: Option<usize>,
}

impl IdeShell {
    pub fn new(
        workspace: Arc<dyn WorkspaceService>,
        initial_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));

        let ws = workspace.clone();
        cx.spawn(async move |this, cx| {
            let root = ws.root().to_owned();
            match build_tree_items(ws, root).await {
                Ok(items) => {
                    _ = this.update(cx, |this, cx| {
                        this.tree_state.update(cx, |state, cx| state.set_items(items, cx));
                    });
                }
                Err(err) => eprintln!("ide: cannot load project tree: {err:#}"),
            }
        })
        .detach();

        let mut this = Self {
            workspace,
            tree_state,
            open: Vec::new(),
            active: None,
        };
        if let Some(path) = initial_file {
            this.open_path(path, window, cx);
        }
        this
    }

    fn open_path(&mut self, path: PathBuf, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.open.iter().position(|file| file.path == path) {
            self.active = Some(ix);
            cx.notify();
            return;
        }

        let read = self.workspace.read_file(&path);
        cx.spawn_in(_window, async move |this, cx| {
            let content = match read.await {
                Ok(content) => content,
                Err(err) => {
                    eprintln!("ide: {err:#}");
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.finish_open(path, content, window, cx);
            });
        })
        .detach();
    }

    fn finish_open(
        &mut self,
        path: PathBuf,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language =
            Language::from_str(path.extension().and_then(|ext| ext.to_str()).unwrap_or(""));

        let editor = cx.new(|cx| {
            InputBaseState::new(window, cx)
                .code_editor(language.name().to_string())
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    hard_tabs: false,
                })
                .default_value(content)
        });

        let subscriptions = vec![
            cx.subscribe(&editor, {
                let path = path.clone();
                move |this: &mut Self, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(file) = this.open.iter_mut().find(|file| file.path == path)
                        && !file.dirty
                    {
                        file.dirty = true;
                        cx.notify();
                    }
                }
            }),
            // The status bar shows the cursor position, which changes without
            // an InputEvent; re-render whenever the editor notifies.
            cx.observe(&editor, |_, _, cx| cx.notify()),
        ];

        let rel: SharedString = path
            .strip_prefix(self.workspace.root())
            .unwrap_or(&path)
            .display()
            .to_string()
            .into();
        let name: SharedString = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_string()
            .into();

        editor.focus_handle(cx).focus(window, cx);
        self.open.push(OpenFile {
            path,
            rel,
            name,
            language,
            editor,
            dirty: false,
            _subscriptions: subscriptions,
        });
        self.active = Some(self.open.len() - 1);
        cx.notify();
    }

    fn save_active(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else { return };
        let file = &self.open[ix];
        if !file.dirty {
            return;
        }

        let text = file.editor.read(cx).text().to_string();
        let write = self.workspace.write_file(&file.path, text);
        let path = file.path.clone();
        cx.spawn(async move |this, cx| {
            match write.await {
                Ok(()) => {
                    _ = this.update(cx, |this, cx| {
                        if let Some(file) = this.open.iter_mut().find(|file| file.path == path) {
                            file.dirty = false;
                        }
                        cx.notify();
                    });
                }
                // The file stays dirty, so nothing is silently lost; surfacing
                // save failures in the UI is a follow-up.
                Err(err) => eprintln!("ide: save failed: {err:#}"),
            }
        })
        .detach();
    }

    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else { return };

        if !self.open[ix].dirty {
            self.remove_tab(ix, cx);
            return;
        }

        let shell = cx.entity();
        let name = self.open[ix].name.clone();
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title(format!("Discard unsaved changes to {name}?"))
                .on_ok({
                    let shell = shell.clone();
                    move |_, _window, cx| {
                        shell.update(cx, |this, cx| {
                            if let Some(ix) = this.active {
                                this.remove_tab(ix, cx);
                            }
                        });
                        true
                    }
                })
        });
    }

    fn remove_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.open.remove(ix);
        self.active = if self.open.is_empty() {
            None
        } else {
            Some(ix.min(self.open.len() - 1))
        };
        cx.notify();
    }

    fn render_project_pane(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                // Tool-window header in the instrumentation voice (rule 5).
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("PROJECT"),
            )
            .child(
                tree(&self.tree_state, move |ix, entry, _selected, _window, cx| {
                    view.update(cx, |_, cx| {
                        let item = entry.item();
                        let icon = if !entry.is_folder() {
                            IconName::File
                        } else if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        };

                        ListItem::new(ix)
                            .w_full()
                            .rounded(cx.theme().radius)
                            .py_0p5()
                            .px_2()
                            .pl(px(14.) * entry.depth() + px(8.))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .whitespace_nowrap()
                                    .child(icon)
                                    .child(item.label.clone()),
                            )
                            .on_click(cx.listener({
                                let item = item.clone();
                                move |this, _, window, cx| {
                                    if item.is_folder() {
                                        return;
                                    }
                                    this.open_path(PathBuf::from(item.id.as_str()), window, cx);
                                }
                            }))
                    })
                })
                .text_sm()
                .p_1()
                .w_full()
                .flex_1(),
            )
    }

    fn render_editor_area(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                TabBar::new("editor-tabs")
                    .underline()
                    .children(self.open.iter().map(|file| {
                        Tab::new().label(if file.dirty {
                            SharedString::from(format!("● {}", file.name))
                        } else {
                            file.name.clone()
                        })
                    }))
                    .selected_index(active.unwrap_or(0))
                    .on_click(cx.listener(|this, ix: &usize, _window, cx| {
                        this.active = Some(*ix);
                        cx.notify();
                    })),
            )
            .child(match active {
                Some(ix) => Input::from_base(&self.open[ix].editor)
                    .bordered(false)
                    .focus_bordered(false)
                    .p_0()
                    .flex_1()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .bg(cx.theme().secondary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("Open a file from the project tree")
                    .into_any_element(),
            })
    }

    fn render_status_bar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let instrument = |text: String| {
            // Instrumentation voice (rule 5): uppercase, small, semibold,
            // muted. Letterspacing is not yet expressible here.
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(text.to_uppercase())
        };

        let mut bar = StatusBar::new();
        if let Some(ix) = self.active {
            let file = &self.open[ix];
            let position = file.editor.read(cx).cursor_position();
            bar = bar
                .left(
                    div()
                        .text_size(px(11.))
                        .text_color(cx.theme().muted_foreground)
                        .child(file.rel.clone()),
                )
                .right(instrument(format!(
                    "{}:{}",
                    position.line + 1,
                    position.character + 1
                )))
                .right(instrument(file.language.name().to_string()));
        }
        bar
    }
}

impl Render for IdeShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("IdeShell")
            .on_action(cx.listener(Self::save_active))
            .on_action(cx.listener(Self::close_active))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_resizable("main-split")
                    .child(
                        resizable_panel()
                            .size(px(280.))
                            .child(self.render_project_pane(window, cx).into_any_element()),
                    )
                    .child(
                        // The borders exception: a 1px line separates the tool
                        // window from the editor column.
                        div()
                            .size_full()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .child(self.render_editor_area(window, cx))
                            .into_any_element(),
                    )
                    .into_any_element(),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.render_status_bar(window, cx)),
            )
    }
}

/// Recursively assemble the project tree through the workspace service. Runs
/// on the foreground executor; the service does its I/O on background threads.
fn build_tree_items(
    ws: Arc<dyn WorkspaceService>,
    dir: PathBuf,
) -> LocalBoxFuture<'static, Result<Vec<TreeItem>>> {
    async move {
        let entries = ws.read_dir(&dir).await?;
        let mut items = Vec::new();
        for entry in entries {
            if entry.is_dir && PRUNED_DIRS.contains(&entry.name.as_str()) {
                continue;
            }
            let id = entry.path.to_string_lossy().to_string();
            if entry.is_dir {
                let children = build_tree_items(ws.clone(), entry.path).await?;
                items.push(TreeItem::new(id, entry.name).children(children));
            } else {
                items.push(TreeItem::new(id, entry.name));
            }
        }
        Ok(items)
    }
    .boxed_local()
}
