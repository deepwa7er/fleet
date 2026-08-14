//! Search-everywhere: the double-shift overlay. A `ListDelegate` over two
//! sections — fuzzy file-path matches (nucleo) and full-text hits (ripgrep's
//! library crates behind `WorkspaceService::search_text`). The component
//! supplies the query input, keyboard navigation, enter → `confirm`, and
//! esc → `cancel`.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, IndexPath,
    list::{ListDelegate, ListItem, ListState},
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};

use crate::app::IdeShell;
use crate::workspace::{TextMatch, WorkspaceService};

const MAX_FILE_HITS: usize = 20;
const MAX_TEXT_HITS: usize = 30;

pub struct SearchDelegate {
    workspace: Arc<dyn WorkspaceService>,
    shell: WeakEntity<IdeShell>,
    matcher: nucleo_matcher::Matcher,
    /// Relative paths of every searchable file, loaded when the overlay opens.
    files: Vec<String>,
    file_hits: Vec<PathBuf>,
    text_hits: Vec<TextMatch>,
    selected: Option<IndexPath>,
    /// Discards text results that arrive for an outdated query.
    generation: usize,
}

impl SearchDelegate {
    pub fn new(workspace: Arc<dyn WorkspaceService>, shell: WeakEntity<IdeShell>) -> Self {
        Self {
            workspace,
            shell,
            matcher: nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths()),
            files: Vec::new(),
            file_hits: Vec::new(),
            text_hits: Vec::new(),
            selected: None,
            generation: 0,
        }
    }

    pub fn set_files(&mut self, files: Vec<PathBuf>) {
        self.files = files
            .into_iter()
            .map(|path| path.display().to_string())
            .collect();
    }

    fn instrument_header(text: &'static str, cx: &App) -> Div {
        // Instrumentation voice (DW-001 rule 5).
        div()
            .px_3()
            .py_1()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }
}

impl ListDelegate for SearchDelegate {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.generation += 1;
        let generation = self.generation;

        if query.trim().is_empty() {
            self.file_hits.clear();
            self.text_hits.clear();
            cx.notify();
            return Task::ready(());
        }

        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        self.file_hits = pattern
            .match_list(self.files.iter().map(String::as_str), &mut self.matcher)
            .into_iter()
            .take(MAX_FILE_HITS)
            .map(|(path, _score)| PathBuf::from(path))
            .collect();
        cx.notify();

        let search = self.workspace.search_text(query.to_string(), MAX_TEXT_HITS);
        cx.spawn(async move |state, cx| {
            let hits = search.await.unwrap_or_default();
            _ = state.update(cx, |state, cx| {
                let delegate = state.delegate_mut();
                if delegate.generation == generation {
                    delegate.text_hits = hits;
                    cx.notify();
                }
            });
        })
    }

    fn sections_count(&self, _: &App) -> usize {
        2
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        match section {
            0 => self.file_hits.len(),
            _ => self.text_hits.len(),
        }
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        Some(Self::instrument_header(
            if section == 0 { "FILES" } else { "TEXT" },
            cx,
        ))
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = match ix.section {
            0 => {
                let path = self.file_hits.get(ix.row)?;
                gpui_component::h_flex()
                    .gap_2()
                    .whitespace_nowrap()
                    .child(IconName::File)
                    .child(path.display().to_string())
            }
            _ => {
                let hit = self.text_hits.get(ix.row)?;
                gpui_component::h_flex()
                    .gap_2()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{}:{}", hit.path.display(), hit.line)),
                    )
                    .child(hit.text.clone())
            }
        };
        Some(
            ListItem::new(ix)
                .w_full()
                .rounded(cx.theme().radius)
                .py_1()
                .px_3()
                .text_sm()
                .child(row),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
        cx.notify();
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(ix) = self.selected else { return };
        let target = match ix.section {
            0 => self
                .file_hits
                .get(ix.row)
                .map(|path| (path.clone(), None)),
            _ => self.text_hits.get(ix.row).map(|hit| {
                let position = lsp_types::Position::new(hit.line.saturating_sub(1), 0);
                (
                    hit.path.clone(),
                    Some(lsp_types::Range::new(position, position)),
                )
            }),
        };
        let Some((rel, goto)) = target else { return };

        let absolute = self.workspace.root().join(rel);
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.update(cx, |this, cx| {
            this.open_path_at(absolute, goto, window, cx);
            this.close_search(window, cx);
        });
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.update(cx, |this, cx| this.close_search(window, cx));
        cx.notify();
    }
}

