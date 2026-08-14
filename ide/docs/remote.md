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
| Document authority | **Server-authoritative + auto-save, both modes; read-only on disconnect** | Joe's guiding principle: as little client-side sync machinery as possible. Replaces an earlier explicit-save draft of this design — deletes drafts, content-hash guards, dirty tracking, and the discard dialog outright. See §6. |

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
| `workspace/listFiles` | `list_files` |
| `workspace/searchText` | `search_text` |

Language methods — the server owns the LSP hub (today's `LspStore`, moved
server-side nearly intact: routing table, root detection, per-document op
chains):

| RPC | Notes |
|---|---|
| `document/didOpen` / `didChange` / `didClose` | notifications; full-text, ordered per document. **`didChange` is the write path**: the server holds the document and auto-saves on idle debounce (§6) |
| `document/save` | request; `ctrl-s` — immediate flush to disk + `didSave` to language servers (keeps save-triggered diagnostics like cargo check meaningful) |
| `language/completion`, `language/hover`, `language/definition` | requests; `lsp_types` passthrough, same types both ends |
| `language/diagnostics` | **server→client notification**, pushed as language servers publish |

Persistence goes exclusively through the document pipeline — slice 5b
removed `write_file` from the seam as dead code; a `workspace/writeFile` RPC
gets re-added if and when non-document tooling needs it.

## 5. The seam refactor (the load-bearing piece)

Today the UI reaches language intelligence through `Entity<LspStore>`
directly; only fs/search go through `WorkspaceService`. M5 unifies them:
**language operations join the `WorkspaceService` trait** (document lifecycle
notifications, the three request methods, and a diagnostics stream —
`BoxStream<(Uri, Vec<Diagnostic>)>`). Consequences:

- `LocalWorkspace` grows an internal LSP hub. `LspStore` sheds its gpui
  `Entity`/`Context` skin (plain struct + channels + a `BackgroundExecutor`
  handle); its op-chain and routing logic move unchanged.
- **The document pipeline is also where auto-save lives** (§6): each
  workspace implementation owns its open documents and persists them on
  idle — locally by writing directly, remotely because the pipeline *is*
  the wire and the server does the same thing on its side. The UI never
  knows how persistence happens.
- `EditorLsp` (the provider bridge installed on editors) calls trait methods
  instead of reading an entity — it stops caring whether the workspace is
  local or remote. `RemoteWorkspace` implements the same methods as RPCs.
- The shell's diagnostics subscription becomes a stream consumer.

This refactor lands as its own PR (slice 5b) with **zero behavior change in
local mode** — that is its acceptance test.

## 6. Document model: server-authoritative, auto-save

Decided by Joe 2026-08-17, replacing an earlier explicit-save draft of this
section. Guiding principle: **as little client-side sync machinery as
possible; the server is the source of truth.**

- **The server owns file content.** The client buffer is a view + edit
  surface. Every edit streams to the server as the existing ordered
  full-text `didChange`; the server holds the document in memory and
  **auto-saves to disk after a ~500ms idle debounce**, and on
  `didClose`/`shutdown`. `ctrl-s` stays as `document/save` — an immediate
  flush + `didSave`, which is also what keeps save-triggered diagnostics
  (cargo check) meaningful.
- **No dirty state.** The tab `●`, the discard-changes dialog, and dirty
  tracking are deleted from the shell. The status bar gains a
  `SYNCED / SYNCING` readout in the instrumentation voice. The only
  per-document client state is the version counter LSP already required.
- **Disconnect → read-only.** If the server is unreachable, editors show a
  banner and stop accepting edits. Bounded auto-reconnect — a few backoff
  retries over ~15s, absorbing the everyday MacBook sleep/wake — then an
  explicit reconnect action. No infinite loop at a usually-powered-off
  desktop.
- **Reconnect re-reads the truth.** A fresh session (per-session lifecycle
  makes it the launch path): `initialize`, re-open each tab from server
  content. No baselines, no hashes, no merge — divergence cannot exist
  because offline editing cannot happen.
- **Loss window: sub-second.** Only edits in flight at the instant the wire
  dies — typically none. Client death (app crash, Mac reboot) loses nothing
  beyond the same window; no draft persistence is needed or designed.
- **Honest costs, accepted:** intermediate states reach disk continuously —
  `git diff` on the desktop shows in-progress edits, and file watchers see
  churn. And an external edit to an *open* file (a server-side `git pull`)
  can be clobbered by the next auto-save. File watching, already on the
  backlog for both modes, is the fix, with the rule: **disk wins when the
  document is idle, typing wins when active.**
- **Local mode behaves identically** — auto-save through the same document
  pipeline — so the two modes never diverge in feel. That change lands
  first, before any remote code (slice 5b).
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

1. **5a — the seam refactor**: language + document ops fold into
   `WorkspaceService`; `LspStore` sheds its gpui skin; local mode
   byte-for-byte behavior-identical. Pure refactor, its own acceptance test.
2. **5b — auto-save (local)**: server-authoritative document model behind
   the trait — idle-debounce persistence, dirty tracking / tab `●` /
   discard dialog deleted, `SYNCED / SYNCING` status readout. Lands before
   any remote code so remote adds no new document semantics.
3. **5c — transport + remote workspace**: shared JSON-RPC module,
   `ide-server` bin, `RemoteWorkspace` covering fs, search, and the document
   pipeline (server-side auto-save), `ide <alias>:<path>` bootstrap,
   integration test over child-process stdio. Language requests return
   empty until 5d. Deliverable: full remote editing, no intellisense yet.
4. **5d — remote language intelligence + reconnect**: LSP hub server-side,
   diagnostics stream over RPC, read-only-on-disconnect with bounded
   auto-retry and the reconnect banner.
5. **5e — Mac onboarding + polish**: build docs, connection status UI,
   fast-fail messages, protocol-version error UX.

## 10. Out of scope (recorded so they stay deliberate)

Incremental document sync (follow-up regardless of remote), file watching /
tree refresh (M4 backlog; when it lands, the auto-save conflict rule is
"disk wins when idle, typing wins when active" — §6), linger/warm-server
mode (§2, only if reindex pain is real), multi-client or collaborative
editing (never a goal), wake-on-LAN for the sleeping desktop.
