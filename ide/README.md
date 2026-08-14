# ide

The fleet's own IDE: a native GPUI app, IntelliJ New UI layout, DW-001 palette.
Decided 2026-08-14 (Fizzy card #51) after ruling out IntelliJ Ultimate
(licensing) and Tauri/webview (we want the pure-Rust build as a long-term
project). Engines are borrowed — tree-sitter grammars, LSP servers,
gpui-component's editor widget — the shell is ours.

## Status — milestone 3 (search-everywhere)

Double-shift (or `ctrl-shift-f`) opens the search overlay: one query, two
sections — fuzzy file-path matches (nucleo, smart-case) and literal full-text
hits with line previews. Enter opens the selection (text hits jump to the
line); esc closes. Backed by `WorkspaceService::{list_files, search_text}`,
implemented with ripgrep's own library crates (`ignore` + `grep-searcher`)
in-process — gitignore respected (`require_git(false)`, so it works before
`git init`), hidden files skipped, binaries detected and skipped, smart-case.
The file index reloads on each overlay open. Double-shift detection watches
modifier transitions with a 400ms window; typing two capitalized words
quickly can false-trigger — acceptable for now, IntelliJ has the same class
of heuristic.

## Previously — milestone 2 (real LSP)

Editors now have language intelligence: diagnostics (squiggles), hover,
completions, and go-to-definition (ctrl-click; cross-file targets open in a
new tab), backed by real language servers.

- The LSP subsystem (`src/lsp/`) is a hand-rolled JSON-RPC-over-stdio client
  on smol primitives — no second async runtime inside gpui. One `LspStore`
  routes documents by extension and workspace root; per-document ops are
  chained so didOpen/didChange/didSave/didClose stay ordered. Sync is
  full-text per change event (correct first; incremental sync is a follow-up).
- Server table: rust-analyzer (`.rs`), ruby-lsp (`.rb`/`.erb`), gopls
  (`.go`), basedpyright (`.py`), vtsls (`.ts`/`.tsx`/`.js`). A missing binary
  degrades to no-LSP with one log line. Currently only rust-analyzer is
  installed on the desktop.
- rust-analyzer root detection understands the fleet's nested workspaces:
  nearest `Cargo.toml` declaring `[workspace]` wins (so `ide/` gets its own
  server instance, service crates share the fleet root's).
- Install gotcha: `rust-analyzer` on PATH is the rustup shim — the component
  must be installed *per toolchain* (`rustup component add rust-analyzer`,
  and again with `--toolchain <pinned>`), or the shim errors out.
- Boundary note: language servers are spawned as local processes directly —
  they must live next to the code, so in milestone 5 this subsystem moves
  server-side behind the `WorkspaceService` seam; the editor-facing provider
  traits are unaffected.

## Previously — milestone 1 (the shell)

`cargo run -- [path]` opens the shell: project tool window, tabbed editors,
status bar. A directory argument becomes the workspace root (default: the
current directory); a file argument roots at its parent with the file open.

- Single-click a file in the tree to open it in a tab; `ctrl-s` saves,
  `ctrl-f4` closes the active tab (with a confirm dialog if dirty — dirty tabs
  show `●`).
- The status bar shows the active file's relative path, cursor position, and
  language in the DW-001 instrumentation voice.
- All fs access goes through the `WorkspaceService` trait (`workspace.rs`) —
  the UI never touches `std::fs`. `LocalWorkspace` is the only implementation
  until milestone 5.
- The DW-001 palette lives in `themes/deepwater.json` (light + dark; light is
  active, following the system appearance is a follow-up). The standing
  DW-001 exception for this app: 1px `border` lines separate panes.
- The project tree prunes `.git`, `.worktrees`, `target`, `node_modules`,
  `tmp` (`PRUNED_DIRS`) — the tree component loads eagerly, so build
  artifacts must not be walked. Revisit if the component grows lazy loading.

### Verifying UI changes headlessly

No graphical session is needed — render under cage's headless backend and
screenshot with grim (both installed on the desktop):

```bash
XDG_RUNTIME_DIR=/run/user/1000 WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
  cage -- ./target/debug/ide <path> &
sleep 10 && XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 grim shot.png
```

Note the stale `wayland-0` socket on the desktop: cage lands on `wayland-1` —
point grim there, not at wayland-0.

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
2. ~~M1 shell~~ — tree, tabs, editor, status bar; `WorkspaceService` seam in
   place (card #52).
3. ~~M2 LSP mux~~ — hand-rolled smol client, five-server table, diagnostics/
   hover/completion/definition (card #53).
4. ~~M3 search-everywhere~~ — double-shift overlay, nucleo + ripgrep crates
   behind the workspace seam (card #54).
5. M4 daily-drive on the desktop.
6. M5 remote: headless `ide-server` on the desktop + native macOS client over
   SSH. **Designed** — see [`docs/remote.md`](docs/remote.md) (card #55):
   per-session `ssh desktop ide-server` over stdio, JSON-RPC reusing the M2
   framing, buffers client-local, language ops folding into the
   `WorkspaceService` seam. Implementation slices 5a–5d, one card each.
