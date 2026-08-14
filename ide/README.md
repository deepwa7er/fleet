# ide

The fleet's own IDE: a native GPUI app, IntelliJ New UI layout, DW-001 palette.
Decided 2026-08-14 (Fizzy card #51) after ruling out IntelliJ Ultimate
(licensing) and Tauri/webview (we want the pure-Rust build as a long-term
project). Engines are borrowed — tree-sitter grammars, LSP servers,
gpui-component's editor widget — the shell is ours.

## Status — milestone 0 (spike)

`cargo run -- [path]` opens one window rendering a source file with
tree-sitter highlighting, line numbers, and indent guides. Defaults to its own
`src/main.rs`.

### Spike findings

- **gpui-component's editor has no LSP client.** The README's "with LSP"
  means it exposes provider traits — `CompletionProvider`, `HoverProvider`,
  `DefinitionProvider`, `CodeActionProvider`, `DocumentColorProvider` — plus a
  `diagnostics_mut()` set the host pushes into (see upstream
  `crates/story/examples/editor.rs`, which wires fake in-memory providers).
  Milestone 2 = implementing those traits backed by real LSP servers. This is
  the right shape for us: the traits sit naturally behind the
  workspace-service seam.
- **gpui is consumed as a git dependency on the zed repo, unpinned by design.**
  gpui-component declares `gpui = { version = "0.2.2", git = ... }` with no
  rev; we must declare it identically or cargo resolves two conflicting gpui
  copies. `Cargo.lock` (committed) is the pin. Upgrades = `cargo update` +
  bumping the gpui-component rev, deliberately, in their own PR.
- **Upstream's `[patch.crates-io]` (psm) is WASM-only** — not replicated here;
  native builds don't need it.

## Why a separate workspace

gpui's dependency tree is enormous and needs GUI system libraries. As a fleet
workspace member it would sit inside every `cargo test --workspace` gate run
for unrelated changes. So `ide/` is its own workspace (excluded in the root
`Cargo.toml`), the same per-project pattern as the Rails apps and tidepool.

**Gates when touching ide/** (run from `ide/`):

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Build deps (Fedora): `fontconfig-devel freetype-devel libxcb-devel
libxkbcommon-devel libxkbcommon-x11-devel wayland-devel vulkan-loader`.

## Roadmap

1. ~~M0 spike~~ — this crate.
2. M1 shell: project tree, tabs, editor pane, status bar — IntelliJ New UI
   layout, DW-001 palette (explicit standing exception: 1px borders as
   separators; the whitespace-only rule doesn't survive editor density). All
   fs/search/LSP access behind a workspace-service trait from day one.
3. M2 LSP mux: rust-analyzer, ruby-lsp, gopls, basedpyright, vtsls via
   async-lsp, routed per workspace root, feeding the provider traits above.
4. M3 search-everywhere (double-shift), backed by ripgrep.
5. M4 daily-drive on the desktop.
6. M5 remote: headless `ide-server` on the desktop + native macOS client over
   SSH. Buffer sync gets designed then — the trait boundary exists so this is
   a second implementation, not a rewrite.
