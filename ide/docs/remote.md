# ide — milestone 5 design: remote (Mac client + ide-server over SSH)

Status: **agreed design, not yet implemented.** Fizzy card #55. Decisions in
§2 were made explicitly by Joe on 2026-08-17; everything else follows from
them or from the architecture protected since milestone 1.

## 1. Topology

```
Mac (deepwater-1)                      Fedora desktop (`ssh desktop`)
┌─────────────────────────┐            ┌──────────────────────────────────┐
│ ide (native GPUI, Metal)│            │ ide-server (per-session, stdio)  │
│  IdeShell — unchanged   │  ssh exec  │  ├─ fs: read/write/walk          │
│  RemoteWorkspace ───────┼────────────┼─►├─ search: ignore+grep crates   │
│  buffers live HERE      │  JSON-RPC  │  └─ LSP hub: rust-analyzer,      │
│  tree-sitter runs HERE  │  over stdio│      ruby-lsp, gopls, …          │
└─────────────────────────┘            │  ~/code/fleet                    │
                                       └──────────────────────────────────┘
```

- The GUI runs **natively on the Mac** (GPUI's home platform). Forwarding the
  Linux GUI (waypipe/X11) was rejected in the original architecture session:
  a GPU-rendered editor over frame streaming is strictly worse than a native
  client on every axis that matters.
- `ide-server` runs **next to the code** because language servers and search
  must: rust-analyzer needs the checkout and toolchain beside it.
- **Buffers and rendering are client-local.** Typing, cursor motion, and
  tree-sitter highlighting never cross the wire. The wire carries: file
  contents at open/save, document-sync notifications for LSP, search queries,
  and language requests (completion/hover/definition) with their responses.

## 2. Decided

| Decision | Choice | Rationale |
|---|---|---|
| Server lifecycle | **Pure per-session** — `ide-server` lives exactly as long as the SSH connection | Simplest model; nothing left running on the desktop. Accepted cost, chosen with eyes open: every IDE launch pays rust-analyzer's fleet reindex (~30–60s; diagnostics warm up progressively). If daily use proves this painful, a linger mode (server survives disconnect ~30 min on a local socket, reattach warm) is the designed follow-up — it changes only the transport bootstrap, nothing in the protocol. |
| Wire format | **JSON-RPC 2.0, `Content-Length` framing over stdio** | Identical framing and correlation machinery to the milestone-2 LSP client — proven code, generalized into a shared transport module used by both. Human-readable frames (debug log flag). Payloads are dominated by file text, so binary encoding buys little; the method surface is encoding-agnostic if that ever changes. |
| Transport & auth | **`ssh <alias> ide-server` via the existing key-based alias** | No ports, no daemon, no new auth surface — rides `~/.ssh/config`. Deliberately *not* Tailscale SSH: its ACL runs in `check` mode, and a periodic browser re-auth would sever a long-lived IDE session. Probes use `-o ConnectTimeout=10 -o BatchMode=yes` so a powered-off desktop (its usual state) fails fast with a clear message, not a hang. |

## 3. Invocation

```bash
ide desktop:code/fleet     # remote: scp-style <ssh-alias>:<path>, ~ -relative
ide ~/code/fleet           # local: unchanged
```

One binary, two bin targets in the ide crate: `ide` (the GUI, both modes) and
`ide-server` (headless, Linux-only in practice). The client runs
`ssh desktop ide-server --stdio` and speaks the protocol over the SSH channel.
`ide-server` must be on the desktop's PATH: `cargo install --path ide --bin
ide-server` from the fleet checkout (login-shell PATH includes
`~/.cargo/bin`).

## 4. Protocol surface

Lifecycle: `initialize` (client sends `PROTOCOL_VERSION: u32` + workspace
path; server replies with its version and the canonicalized root — mismatch
is an instructive error: "rebuild ide-server: ssh desktop 'cd ~/code/fleet &&
git pull && cargo install --path ide --bin ide-server'"), then `initialized`,
then traffic; `shutdown` on quit.

Workspace methods — exactly the `WorkspaceService` trait, 1:1:

| RPC | Maps to |
|---|---|
| `workspace/readDir` | `read_dir` |
| `workspace/readFile` | `read_file` |
| `workspace/writeFile` | `write_file` |
| `workspace/listFiles` | `list_files` |
| `workspace/searchText` | `search_text` |

Language methods — the server owns the LSP hub (today's `LspStore`, moved
server-side nearly intact: routing table, root detection, per-document op
chains):

| RPC | Notes |
|---|---|
| `document/didOpen` / `didChange` / `didSave` / `didClose` | notifications; full-text sync, same as today |
| `language/completion`, `language/hover`, `language/definition` | requests; `lsp_types` passthrough, same types both ends |
| `language/diagnostics` | **server→client notification**, pushed as language servers publish |

## 5. The seam refactor (the load-bearing piece)

Today the UI reaches language intelligence through `Entity<LspStore>`
directly; only fs/search go through `WorkspaceService`. M5 unifies them:
**language operations join the `WorkspaceService` trait** (document lifecycle
notifications, the three request methods, and a diagnostics stream —
`BoxStream<(Uri, Vec<Diagnostic>)>`). Consequences:

- `LocalWorkspace` grows an internal LSP hub. `LspStore` sheds its gpui
  `Entity`/`Context` skin (plain struct + channels + a `BackgroundExecutor`
  handle); its op-chain and routing logic move unchanged.
- `EditorLsp` (the provider bridge installed on editors) calls trait methods
  instead of reading an entity — it stops caring whether the workspace is
  local or remote. `RemoteWorkspace` implements the same methods as RPCs.
- The shell's diagnostics subscription becomes a stream consumer.

This refactor lands as its own PR (slice 5b) with **zero behavior change in
local mode** — that is its acceptance test.

## 6. Buffer model & failure modes

- Editing is client-local; the file is only written on explicit save
  (`writeFile` + `didSave`). There is no collaborative sync problem: single
  user, single client, save-based persistence. The CRDT question from the
  original design session dissolves under these constraints.
- **Connection drop loses nothing.** Dirty buffers are client memory; saves
  fail visibly and the buffer stays dirty. Reconnect = new session (per-
  session lifecycle makes this the same code path as launch): re-`initialize`,
  re-`didOpen` every open tab, language servers re-warm. A status-bar
  indicator shows connection state; reconnect is explicit (a click/action),
  not a silent retry loop.
- Desktop powered off at launch: fail within 10s with the machine's actual
  state ("desktop is unreachable — it is usually powered off; check
  `tailscale status`").

## 7. Mac onboarding (one-time)

Rust via rustup, Xcode CLT for Metal/linking, shallow clone of the fleet,
`cargo build` in `ide/`. The Mac already runs cargo-built launchd agents, so
the toolchain likely exists. No cross-compilation story — building the mac
client on the Mac is the only honest path for a gpui app today.

## 8. Testing strategy

- The transport/correlation core stays under the existing unit tests.
- **Integration tests without SSH**: `cargo test` spawns `ide-server` as a
  child process and drives a real `RemoteWorkspace` over its stdio — the full
  RPC path, no network, no display. This is the main safety net.
- SSH-path smoke test from this session: `ide-server` reached via
  `ssh desktop` *from the desktop itself* exercises the real bootstrap.
- UI verification stays cage+grim; the Mac end is verified by Joe.

## 9. Slices (one card = one branch = one PR each)

1. **5a — transport + fs/search remote**: shared JSON-RPC module, `ide-server`
   bin serving the workspace methods, `RemoteWorkspace`, `ide <alias>:<path>`
   bootstrap, integration test. Deliverable: remote tree/open/edit/save/search
   with no language intelligence.
2. **5b — the seam refactor**: language ops fold into `WorkspaceService`;
   local mode byte-for-byte behavior-identical.
3. **5c — remote language intelligence**: LSP hub server-side, diagnostics
   stream over RPC, reconnect flow.
4. **5d — Mac onboarding + polish**: build docs, connection status UI,
   fast-fail messages, protocol-version error UX.

## 10. Out of scope (recorded so they stay deliberate)

Incremental document sync (follow-up regardless of remote), file watching /
tree refresh (M4 backlog), linger/warm-server mode (§2, only if reindex pain
is real), multi-client or collaborative editing (never a goal), wake-on-LAN
for the sleeping desktop.
