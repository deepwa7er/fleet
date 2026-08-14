//! The shell: IntelliJ New UI layout — project tool window, tab strip, editor
//! pane, status bar — in the DW-001 palette (see themes/deepwater.json).
//! DW-001 note: the fleet style guide bans separator borders (rule 1), but the
//! IDE carries a standing exception — 1px `theme.border` lines separate panes,
//! because whitespace alone cannot hold this density. Everything else follows
//! the guide: paper chrome, the editor as a `--fill` recess (rule 2), accent
//! only on interactive/selection states (rule 4), instrumentation voice for
//! the tool-window header and status bar (rule 5).

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::future::LocalBoxFuture;
use futures::{FutureExt as _, StreamExt as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, h_flex,
    highlighter::{Diagnostic, DiagnosticSeverity, Language},
    input::{Input, InputBaseState, InputEvent, Position, TabSize},
    list::{ListItem, ListState},
    resizable::{h_resizable, resizable_panel},
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    tree::{TreeItem, TreeState, tree},
    v_flex,
};

use ide::lsp::uri_to_path;
use ide::workspace::{ConnectionEvent, WorkspaceService};

use crate::providers::EditorLsp;
use crate::search::SearchDelegate;

actions!(ide, [Save, CloseTab, OpenSearch, Reconnect]);

/// The shell's view of the remote connection; local mode never leaves
/// `Connected` (docs/remote.md §6).
#[derive(Clone, Copy, PartialEq)]
enum ConnState {
    Connected,
    Reconnecting,
    Down,
}

/// Two shift taps this close together open search-everywhere.
const DOUBLE_SHIFT_WINDOW: Duration = Duration::from_millis(400);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-s", Save, Some("IdeShell")),
        // IntelliJ's close-tab chord; ctrl-w belongs to the editor's own map.
        KeyBinding::new("ctrl-f4", CloseTab, Some("IdeShell")),
        // Chord fallback for search-everywhere; double-shift is the reflex.
        KeyBinding::new("ctrl-shift-f", OpenSearch, Some("IdeShell")),
        // Manual reconnect once the automatic round gives up.
        KeyBinding::new("ctrl-shift-r", Reconnect, Some("IdeShell")),
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
    _subscriptions: Vec<Subscription>,
}

pub struct IdeShell {
    workspace: Arc<dyn WorkspaceService>,
    /// Focused whenever no editor is — keystrokes (and modifier events, which
    /// travel the focus path) must always have somewhere to land, or
    /// double-shift is dead until the first file opens.
    focus_handle: FocusHandle,
    tree_state: Entity<TreeState>,
    open: Vec<OpenFile>,
    active: Option<usize>,
    search: Option<Entity<ListState<SearchDelegate>>>,
    prev_shift: bool,
    last_shift_tap: Option<Instant>,
    /// Auto-save state for the status-bar readout: true = all flushed.
    synced: bool,
    connection: ConnState,
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

        // Diagnostics flow from the workspace as a plain stream now that the
        // language hub lives behind the seam; consume it for the window's
        // lifetime.
        if let Some(mut diagnostics) = workspace.subscribe_diagnostics() {
            cx.spawn(async move |this, cx| {
                while let Some((path, diagnostics)) = diagnostics.next().await {
                    let alive = this.update(cx, |this, cx| {
                        this.apply_diagnostics(&path, &diagnostics, cx);
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        if let Some(mut connection) = workspace.subscribe_connection() {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(event) = connection.next().await {
                    let alive = this.update_in(cx, |this, window, cx| {
                        this.connection = match event {
                            ConnectionEvent::Lost => ConnState::Down,
                            ConnectionEvent::Reconnecting => ConnState::Reconnecting,
                            ConnectionEvent::Restored => ConnState::Connected,
                        };
                        if event == ConnectionEvent::Restored {
                            // Fresh session: re-read every open tab from the
                            // server — its content is the truth (§6).
                            this.refresh_open_documents(window, cx);
                        }
                        cx.notify();
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        if let Some(mut sync_state) = workspace.subscribe_sync_state() {
            cx.spawn(async move |this, cx| {
                while let Some(synced) = sync_state.next().await {
                    let alive = this.update(cx, |this, cx| {
                        this.synced = synced;
                        cx.notify();
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }

        let focus_handle = cx.focus_handle();
        window.defer(cx, {
            let focus_handle = focus_handle.clone();
            move |window, cx| focus_handle.focus(window, cx)
        });

        let mut this = Self {
            workspace,
            focus_handle,
            tree_state,
            open: Vec::new(),
            active: None,
            search: None,
            prev_shift: false,
            last_shift_tap: None,
            synced: true,
            connection: ConnState::Connected,
        };
        if let Some(path) = initial_file {
            this.open_path(path, window, cx);
        }
        this
    }

    fn apply_diagnostics(
        &mut self,
        path: &Path,
        diagnostics: &[lsp_types::Diagnostic],
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.open.iter().find(|file| file.path == path) else {
            return;
        };
        let converted: Vec<Diagnostic> = diagnostics.iter().map(convert_diagnostic).collect();
        file.editor.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.clear();
                set.extend(converted);
            }
            cx.notify();
        });
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path_at(path, None, window, cx);
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(window, cx);
            return;
        }
        let delegate = SearchDelegate::new(self.workspace.clone(), cx.entity().downgrade());
        let state = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        state.update(cx, |state, cx| state.focus(window, cx));

        // The file index loads freshly on every open — cheap at fleet scale.
        let files = self.workspace.list_files();
        let for_search = state.clone();
        cx.spawn(async move |_, cx| {
            let Ok(files) = files.await else { return };
            for_search.update(cx, |state, cx| {
                state.delegate_mut().set_files(files);
                cx.notify();
            });
        })
        .detach();

        self.search = Some(state);
        cx.notify();
    }

    pub(crate) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        match self.active {
            Some(ix) => self.open[ix].editor.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
        cx.notify();
    }

    fn on_open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
    }

    fn on_reconnect(&mut self, _: &Reconnect, _window: &mut Window, _cx: &mut Context<Self>) {
        self.workspace.reconnect();
    }

    /// After a restored session, adopt server truth into every open editor
    /// and re-open the documents server-side.
    fn refresh_open_documents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for file in &self.open {
            let path = file.path.clone();
            let editor = file.editor.clone();
            let read = self.workspace.read_file(&path);
            let workspace = self.workspace.clone();
            cx.spawn_in(window, async move |_, cx| {
                let Ok(text) = read.await else { return };
                workspace.document_open(&path, text.clone());
                _ = editor.update_in(cx, |state, window, cx| {
                    state.set_value(text, window, cx);
                });
            })
            .detach();
        }
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shift_only = event.modifiers.shift
            && !event.modifiers.control
            && !event.modifiers.alt
            && !event.modifiers.platform
            && !event.modifiers.function;

        if shift_only && !self.prev_shift {
            let now = Instant::now();
            if self
                .last_shift_tap
                .take()
                .is_some_and(|tap| now.duration_since(tap) < DOUBLE_SHIFT_WINDOW)
            {
                if self.search.is_none() {
                    self.open_search(window, cx);
                }
            } else {
                self.last_shift_tap = Some(now);
            }
        }
        self.prev_shift = event.modifiers.shift;
    }

    /// Open (or focus) `path`, optionally jumping to `goto` once it is open —
    /// the cross-file half of go-to-definition.
    pub(crate) fn open_path_at(
        &mut self,
        path: PathBuf,
        goto: Option<lsp_types::Range>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.open.iter().position(|file| file.path == path) {
            self.active = Some(ix);
            if let Some(range) = goto {
                self.open[ix].editor.update(cx, |state, cx| {
                    state.set_cursor_position(
                        Position::new(range.start.line, range.start.character),
                        window,
                        cx,
                    );
                });
            }
            self.open[ix].editor.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }

        let read = self.workspace.read_file(&path);
        cx.spawn_in(window, async move |this, cx| {
            let content = match read.await {
                Ok(content) => content,
                Err(err) => {
                    eprintln!("ide: {err:#}");
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.finish_open(path, content, goto, window, cx);
            });
        })
        .detach();
    }

    fn finish_open(
        &mut self,
        path: PathBuf,
        content: String,
        goto: Option<lsp_types::Range>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language =
            Language::from_str(path.extension().and_then(|ext| ext.to_str()).unwrap_or(""));

        self.workspace.document_open(&path, content.clone());
        let bridge = Rc::new(EditorLsp::new(self.workspace.clone(), path.clone()));

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

        let shell = cx.entity().downgrade();
        editor.update(cx, |state, _| {
            // Installed unconditionally: the workspace answers with empty
            // results for languages it has no server for.
            state.lsp.completion_provider = Some(bridge.clone());
            state.lsp.hover_provider = Some(bridge.clone());
            state.lsp.definition_provider = Some(bridge.clone());
            // Cross-file go-to-definition: the shell opens the target tab;
            // same-file jumps fall through to the editor's own handling.
            let own_path = path.clone();
            state.lsp.show_document = Some(std::rc::Rc::new(
                move |params: &lsp_types::ShowDocumentParams, window, cx| {
                    let Some(target) = uri_to_path(&params.uri) else {
                        return false;
                    };
                    if target == own_path {
                        return false;
                    }
                    let Some(shell) = shell.upgrade() else {
                        return false;
                    };
                    let selection = params.selection;
                    shell.update(cx, |this, cx| {
                        this.open_path_at(target, selection, window, cx);
                    });
                    true
                },
            ));
        });

        let subscriptions = vec![
            cx.subscribe(&editor, {
                let path = path.clone();
                move |this: &mut Self, editor, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = editor.read(cx).text().to_string();
                        this.workspace.document_changed(&path, text);
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
        if let Some(range) = goto {
            editor.update(cx, |state, cx| {
                state.set_cursor_position(
                    Position::new(range.start.line, range.start.character),
                    window,
                    cx,
                );
            });
        }
        self.open.push(OpenFile {
            path,
            rel,
            name,
            language,
            editor,
            _subscriptions: subscriptions,
        });
        self.active = Some(self.open.len() - 1);
        cx.notify();
    }

    /// ctrl-s: auto-save persists everything anyway; this is the explicit
    /// "flush now and tell the tools" (didSave → cargo check etc.).
    fn save_active(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else { return };
        let file = &self.open[ix];
        let text = file.editor.read(cx).text().to_string();
        let save = self.workspace.document_save(&file.path, text);
        cx.spawn(async move |_, _| {
            if let Err(err) = save.await {
                // Auto-save will retry on the next edit; surfacing write
                // failures in the UI is a follow-up.
                eprintln!("ide: save failed: {err:#}");
            }
        })
        .detach();
    }

    /// Closing needs no confirmation: the workspace flushes on close, so
    /// there is nothing to discard.
    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else { return };
        self.remove_tab(ix, window, cx);
    }

    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.document_closed(&self.open[ix].path);
        self.open.remove(ix);
        self.active = if self.open.is_empty() {
            // Keystrokes need a home once the last editor is gone.
            self.focus_handle.focus(window, cx);
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
        let connected = self.connection == ConnState::Connected;
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(!connected, |this| {
                // Read-only until the session is back (docs/remote.md §6).
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().danger)
                        .child(match self.connection {
                            ConnState::Reconnecting => "DISCONNECTED — RECONNECTING…",
                            _ => "DISCONNECTED — CTRL-SHIFT-R TO RECONNECT",
                        }),
                )
            })
            .child(
                TabBar::new("editor-tabs")
                    .underline()
                    .children(
                        self.open
                            .iter()
                            .map(|file| Tab::new().label(file.name.clone())),
                    )
                    .selected_index(active.unwrap_or(0))
                    .on_click(cx.listener(|this, ix: &usize, _window, cx| {
                        this.active = Some(*ix);
                        cx.notify();
                    })),
            )
            .child(match active {
                Some(ix) => Input::from_base(&self.open[ix].editor)
                    .readonly(!connected)
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
        // Auto-save readout (docs/remote.md §6): SYNCED when every open
        // document is flushed. Instrumentation, not an alarm — muted either way.
        bar = bar.right(instrument(
            if self.synced { "synced" } else { "syncing…" }.to_string(),
        ));
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
        let main = v_flex()
            .size_full()
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
            );

        div()
            .size_full()
            .relative()
            .key_context("IdeShell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::save_active))
            .on_action(cx.listener(Self::close_active))
            .on_action(cx.listener(Self::on_open_search))
            .on_action(cx.listener(Self::on_reconnect))
            .on_modifiers_changed(cx.listener(|this, event, window, cx| {
                this.on_modifiers_changed(event, window, cx);
            }))
            .child(main)
            .when_some(self.search.clone(), |this, search| {
                this.child(self.render_search_overlay(&search, cx))
            })
    }
}

impl IdeShell {
    /// The search overlay. The backdrop occludes everything beneath it — so
    /// scrolling in the results can never scroll the editor below — and a
    /// click outside the panel dismisses it. No shadow: DW-001 reserves depth
    /// for pressable things; the 1px border (the IDE's standing exception)
    /// does the lifting.
    fn render_search_overlay(
        &self,
        search: &Entity<ListState<SearchDelegate>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_search(window, cx)),
            )
            .flex()
            .items_start()
            .justify_center()
            .pt(px(96.))
            .child(
                div()
                    .w(px(680.))
                    .h(px(440.))
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .overflow_hidden()
                    .occlude()
                    // Keep clicks inside the panel from reaching the
                    // backdrop's close handler.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(gpui_component::list::List::new(search)),
            )
    }
}

fn convert_diagnostic(diagnostic: &lsp_types::Diagnostic) -> Diagnostic {
    let start = Position::new(diagnostic.range.start.line, diagnostic.range.start.character);
    let end = Position::new(diagnostic.range.end.line, diagnostic.range.end.character);
    let severity = match diagnostic.severity {
        Some(lsp_types::DiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => DiagnosticSeverity::Info,
        Some(lsp_types::DiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
        // Errors, and the spec's "client decides" case.
        _ => DiagnosticSeverity::Error,
    };
    let mut converted =
        Diagnostic::new(start..end, diagnostic.message.clone()).with_severity(severity);
    if let Some(source) = &diagnostic.source {
        converted = converted.with_source(source.clone());
    }
    converted
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
