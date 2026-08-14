//! Milestone 0 spike: prove the gpui + gpui-component stack builds and renders
//! a fleet source file with tree-sitter highlighting on this machine.
//!
//! Usage: `cargo run -- [path]` — defaults to this file.

use std::path::PathBuf;

use gpui::*;
use gpui_component::{
    ActiveTheme as _, Root,
    highlighter::Language,
    input::{Input, InputBaseState, TabSize},
};
use gpui_component_assets::Assets;

struct Spike {
    editor: Entity<InputBaseState>,
}

impl Render for Spike {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().background).child(
            Input::from_base(&self.editor)
                .bordered(false)
                .focus_bordered(false)
                .p_0()
                .h_full()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size),
        )
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"));

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            eprintln!("ide: cannot read {}: {err}", path.display());
            std::process::exit(1);
        }
    };

    let language =
        Language::from_str(path.extension().and_then(|ext| ext.to_str()).unwrap_or(""));
    let title = format!(
        "{} — ide",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("ide")
    );

    gpui_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            cx.activate(true);

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1200.), px(800.)),
                    cx,
                ))),
                ..Default::default()
            };

            cx.spawn(async move |cx| {
                cx.open_window(options, |window, cx| {
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
                    let spike = cx.new(|_| Spike { editor });
                    cx.new(|cx| Root::new(spike, window, cx).bg(cx.theme().background))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}
