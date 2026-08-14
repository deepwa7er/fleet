//! `EditorLsp`: adapts the `WorkspaceService` language methods onto
//! gpui-component's editor provider traits. Holds only the workspace handle
//! and a path — it neither knows nor cares whether the workspace is local or
//! remote, which is the whole point of the seam (docs/remote.md §5).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gpui::{App, Context, Task, Window};
use gpui_component::input::{
    CompletionProvider, DefinitionProvider, HoverProvider, InputBaseState, Rope, RopeExt as _,
};

use ide::workspace::WorkspaceService;

pub struct EditorLsp {
    workspace: Arc<dyn WorkspaceService>,
    path: PathBuf,
}

impl EditorLsp {
    pub fn new(workspace: Arc<dyn WorkspaceService>, path: PathBuf) -> Self {
        Self { workspace, path }
    }
}

impl CompletionProvider for EditorLsp {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        cx: &mut Context<InputBaseState>,
    ) -> Task<Result<lsp_types::CompletionResponse>> {
        let request = self
            .workspace
            .completion(&self.path, rope.offset_to_position(offset), trigger);
        cx.background_executor().spawn(request)
    }

    fn inline_completion(
        &self,
        _rope: &Rope,
        _offset: usize,
        _trigger: lsp_types::InlineCompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputBaseState>,
    ) -> Task<Result<lsp_types::InlineCompletionResponse>> {
        Task::ready(Ok(lsp_types::InlineCompletionResponse::Array(vec![])))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        new_text: &str,
        _cx: &mut Context<InputBaseState>,
    ) -> bool {
        let Some(last) = new_text.chars().last() else {
            return false;
        };
        if last.is_alphanumeric() || last == '_' {
            return true;
        }
        self.workspace
            .completion_triggers(&self.path)
            .iter()
            .any(|trigger| trigger.as_str() == last.to_string())
    }
}

impl HoverProvider for EditorLsp {
    fn hover(
        &self,
        rope: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let request = self
            .workspace
            .hover(&self.path, rope.offset_to_position(offset));
        cx.background_executor().spawn(request)
    }
}

impl DefinitionProvider for EditorLsp {
    fn definitions(
        &self,
        rope: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<lsp_types::LocationLink>>> {
        let request = self
            .workspace
            .definition(&self.path, rope.offset_to_position(offset));
        cx.background_executor().spawn(request)
    }
}
