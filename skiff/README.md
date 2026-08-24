# skiff

> **Being rebuilt.** Skiff is becoming one Rust binary plus a React client
> ([DW-004](../docs/skiff-architecture.md)); the Rails app and the Node bridge
> below are both deleted at cutover. Both stacks live here and run side by side
> until then — the live one on **8120**, `skiffd` on **8121** — which is safe
> because everything skiffd caches is derived from the same files. See
> [The rebuild](#the-rebuild) before changing anything in `src/` or `web/`.

A phone-optimized web UI for coding-agent sessions across three harnesses —
pi, Meta's Muse Code, and opencode — behind one bridge. Browse sessions from
every harness in one list, read transcripts, send messages, watch replies
stream live, and abort a run — all from a phone browser over the tailnet.
The app is deployable to either of two tailnet hosts: the Fedora desktop
(`fedora`, the current home) and the Mac (`deepwater-1`, still running until
retired).

---

## The rebuild

`src/` (Rust) and `web/` (React) are the replacement for `app/` (Rails) and
`bridge/` (Node). The design, including the alternatives that were rejected and
why, is [DW-004](../docs/skiff-architecture.md) — read it before building any
of it.

The one-sentence version: **skiff is a live read model over state that other
processes own, plus a small set of commands.** Harness formats die in `ingest`;
a derived SQLite store is the only thing views query; every read the client
performs is a subscription over one WebSocket; and `Rust owns truth,
derivation, and consistency, React owns intent and presentation`.

### Run it

```sh
cd web && bun install && bun run build   # nothing else typechecks the client
cargo run -p skiff
```

Then open `http://127.0.0.1:8121`. Everything has a working default: the read
model lands in `$XDG_STATE_HOME/skiff`, and the pi session directory resolves
the way pi itself resolves it. `--help` lists the overrides.

For client work, `cd web && bun run dev` serves the client with hot reload and
proxies `/ws` to a `cargo run -p skiff` on 8121.

The read model is **derived, and safe to delete at any time** — it is rebuilt
on the next scan. There are no migrations: the schema carries a version, and a
mismatch drops every table and re-ingests.

### Gates

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
(cd skiff/web && bun run build)
```

The client's types are generated from the Rust wire types, and drift is a
failing test. To accept a protocol change:

```sh
SKIFF_WRITE_TYPES=1 cargo test -p skiff --test types
```

`web/src/gen/` holds **nothing that was not generated** — the test compares its
whole contents against a fresh export, and regenerating deletes anything it
does not recognise. The hand-written barrel is `web/src/types.ts`, beside it.

### Where things are

| | |
|---|---|
| `src/model.rs` | the domain vocabulary; the leaf-path walk that makes a tree a transcript |
| `src/ingest/` | one adapter per source, the watermark, the scan loop, the topics |
| `src/store/` | the derived SQLite read model, and the rebuild that replaces migrations |
| `src/views/` | the closed set of live queries and what invalidates each |
| `src/wire.rs` | the client protocol; every type here is exported to TypeScript |
| `src/server.rs` | one task per connection: the socket, its subscriptions, its topics |
| `web/src/lib/socket.ts` | the whole client data layer — subscriptions, reconnect, resubscribe |

### Status

M1 of [DW-004 §13](../docs/skiff-architecture.md): the skeleton, and pi
sessions end to end — ingest, store, the `sessions` live query, and a React
shell that lists them. The transcript, the other two harnesses, changes, and
the desk are M2–M6; the flip at breakwater is M7.

---

## The stack being replaced

Everything below describes the Rails + Node bridge stack, which is still the
one serving `agent.intern.deepwa7er.net`.

## Architecture

```
phone (Tailscale) ──https──> breakwater (TLS) ──http──> Rails on fedora:8120 (tailnet IP)
                                                          │
                                              SSE proxy (sessions#stream)
                                                          │  streaming HTTP, basic auth
                                                          ▼
                                            skiff bridge on 127.0.0.1:4120
                                              │               │               │
                                     pi harness       muse harness     opencode harness
                                     `pi --mode rpc`  `muse exec       `opencode serve`
                                     + session files   --json --yolo`   on 127.0.0.1:4130
                                          │           + session files         │
                                          ▼                 ▼                 ▼
                                 ~/.pi/agent/sessions  ~/.local/share/   opencode's own
                                 (the pi CLI's own     muse/sessions     session store
                                  sessions)            (the muse CLI's
                                                        own sessions)
```

Session ids on the wire are harness-qualified — `pi:…`, `muse:…`,
`opencode:…` — and every session carries its `harness` and a `capabilities`
object, so the UI renders exactly the controls each harness supports (rename,
the orchestrator toggle, and the model picker are not universal; pi sessions
can switch models from the phone via pi's `set_model`, which also keeps the
choice as pi's default). Sessions are the same ones
the respective CLIs drive: a session started at a terminal shows up on the
phone, and vice versa.

The session view streams: the bridge's generic registry
(bridge/lib/stream-registry.js) holds a per-session live state — the mapped
transcript, the in-flight assistant overlay, the orchestrator readout — fed
by a per-harness driver, and pushes it over SSE. Rails proxies that stream
and translates each event into turbo-streams; the browser renders them with
Turbo.renderStreamMessage. No polling, no fingerprints, no position
bookkeeping: every connect (and every reconnect) starts with a snapshot that
replaces the transcript, so the DOM always converges.

Rails is the only consumer of the bridge and reaches it over loopback.

Design decisions (recorded so they are not re-litigated later):

- **TLS terminated by breakwater.** The public URL
  `https://agent.intern.deepwa7er.net` is served by breakwater, which
  terminates TLS and forwards plain HTTP to Rails. `config.assume_ssl` is on
  so Rails generates https URLs behind the proxy; `config.force_ssl` is off
  because breakwater 308-redirects HTTP itself and direct tailnet http access
  still works. SSE must pass through unbuffered — verify breakwater's proxy
  buffering before changing anything there.
- **Explicit Host allowlist.** `config.hosts` in production.rb is exactly
  `agent.intern.deepwa7er.net` (breakwater forwards the inbound Host verbatim),
  `fedora.tailcfab97.ts.net`, `deepwater-1.tailcfab97.ts.net`, `localhost`,
  `127.0.0.1` — both MagicDNS names, because the app deploys to either host.
  Everything else — including poking at the tailnet IP directly — gets a 403.
- **Secrets live in one file.** `~/.config/skiff/secrets`, loaded by
  `config/initializers/skiff_env.rb`; the skiff deploy wrapper never reads it.
  It holds one consumer's credential — the bridge's basic-auth password
  (`SKIFF_BRIDGE_PASSWORD`). The path resolves from the home directory
  (`SKIFF_SECRETS_FILE` overrides) so the same file location works on the Mac
  and the desktop.
- **Ports.** Rails: 8120 on the tailnet IP. Bridge: 127.0.0.1:4120 (the
  bridge API's contract; the Rails client's default URL is unchanged).
  opencode serve: 127.0.0.1:4130, its own systemd user unit
  (deploy/opencode-serve.service) — opencode is itself a session server, so
  the bridge connects to it instead of spawning per-session processes.
- **One bridge, per-harness adapters.** The HTTP surface, auth, the stream
  registry, and the `{ info, parts }` transcript shape are harness-agnostic;
  everything else lives behind one adapter interface per harness
  (bridge/lib/pi-harness.js, muse-harness.js, opencode-harness.js). A
  harness whose binary is missing on a host degrades to a named error in the
  session list — never a dead bridge, never silently absent sessions.
- **The desk is the root; the session is the container.** DW-002 §6: root is
  one page ordered by what needs you — changes in review, then working, then
  idle — with the sessions list surviving at /sessions. The change the
  agent bound to a session comes back to that session: the session page
  renders the same review region as the change page (one shared partial —
  the header's claims, the latest round's annotated diff, and the
  approve/request-changes verbs, which return to the session rather than
  navigating away), so the whole loop — ask, implement, review, approve,
  next round — stays in the chat that started it. The change page keeps the
  full view: rounds navigation, the cumulative diff, and the bound session
  embedded below.
- **muse runs headless per prompt.** Muse has no long-lived RPC mode; the
  bridge spawns one `muse exec --json --yolo --session-id <uuid>` child per
  prompt (the prompt travels by file, never argv), reads incremental
  `run.output.delta` events off stdout for the live overlay, and reads
  committed messages from the session file only — exec stdout never carries
  them. `--yolo` is deliberate: approval prompts have no human on the other
  end of this bridge, the same trust model as driving pi from the phone.
  Muse names its own sessions (no rename), and persists reasoning encrypted
  (it never renders).
- **The session view streams, deliberately.** DW-001 §6 originally chose
  polling (phone battery discipline: the poller went radio-silent when the
  session was idle). The stream reverses that trade: one open SSE connection
  per page view, for the page's lifetime — simpler (no position protocol, no
  fingerprints, no backoff) and smoother (coalesced ~100ms overlay pushes
  instead of 300ms poll turns). The battery cost is accepted: the phone is
  not the primary client. Each open stream occupies one Puma thread
  (`RAILS_MAX_THREADS`, default 5, is the knob; one or two viewers fit
  comfortably) — which makes releasing it the thing that matters. A thread
  blocked reading the bridge never sees breakwater drop the browser's side,
  so the bridge ticks a `heartbeat` event every 15s on every stream: the
  proxy forwards each tick as an SSE comment (the write is what raises
  `ClientDisconnected` for a viewer that left), and bounds its upstream read
  at three intervals (a silent bridge frees the thread too). Before this,
  five closed tabs drained the pool and every request hung (card #102).
- **Reconciliation lives in the bridge, not the view.** The registry decides
  what changed — file appends, overlay growth, overlay resolution, aborted
  runs, compaction — and emits exact append/replace/remove ops. The view
  renders ops; it never diffs. The one structural judgment per harness is
  when a newly-appended entry resolves the overlay: for pi any assistant
  entry (pi persists the assistant entry exactly at `message_end`, and
  nothing else can append while the overlay streams); for muse an assistant
  entry with text (tool-call batches commit mid-run without ending the
  streamed output). opencode needs no overlay at all — it updates the
  in-flight message inside its own message list, so its driver refetches and
  lets the registry diff.

## Setup (macOS)

Prerequisites on the Mac:

- Ruby 4.0.6 via mise (the repo's `.ruby-version`).
- The skiff bridge at `127.0.0.1:4120`, run by a launchd agent (installed
  separately, out of repo; its wrapper is `~/.config/skiff/skiff-bridge.sh`,
  installed out-of-band on the Mac — the repo's `deploy/install-desktop.sh`
  covers the desktop).
- The secrets file `~/.config/skiff/secrets`, containing the bridge's
  basic-auth password (`SKIFF_BRIDGE_PASSWORD`).

Then:

```sh
bundle install
bin/setup
```

## Run

Development:

```sh
bin/rails server -p 3000
```

Production on macOS (runs forever under launchd, bound to the tailnet IP on
port 8120):

```sh
deploy/install-agent.sh   # installs com.deepwa7er.skiff; safe to re-run
```

The skiff agent is installed by that script; the bridge agent is installed
separately (out of repo). Uninstall with `deploy/uninstall-agent.sh`.

On the Fedora desktop the same role is filled by systemd user units — see
"Deploy to the desktop (Fedora)" below.

## Deploy to the desktop (Fedora)

The desktop runs skiff as systemd **user** units (`skiff.service`,
`skiff-bridge.service`, and `opencode-serve.service`) — everything lives
under `$HOME`, no sudo. This is the current primary deployment.

Prerequisites, shipped out-of-band (never in git):

- Ruby 4.0.6 (the desktop's system Ruby, resolved through PATH — no mise).
- Node ≥22 (the desktop's `/usr/bin/node`).
- The `pi` CLI (`~/.local/bin/pi`) and the `muse` CLI (`~/.local/bin/muse`);
  `opencode` at `~/.opencode/bin/opencode` for the opencode-serve unit. A
  missing harness degrades to a named error in the session list.
- The secrets file `~/.config/skiff/secrets` (same format as the Mac's:
  `SKIFF_BRIDGE_PASSWORD`).
- The repo's `config/master.key`, copied onto the box.

First install:

```sh
cd ~/code/fleet/skiff
bundle install
bin/rails assets:precompile
deploy/install-desktop.sh   # installs all three user units; safe to re-run
```

(`install-desktop.sh` also retires the pre-multi-harness unit name,
`com.deepwa7er.pi-bridge.service`, if the host still carries it.)

Deploying an update:

```sh
git pull && systemctl --user restart skiff skiff-bridge opencode-serve
```

Uninstall with `deploy/uninstall-desktop.sh`.

## Remote power-on (Wake-on-LAN)

The desktop is usually powered off. To bring it back — and skiff with it —
from anywhere, Wake-on-LAN gets the NIC to power the machine on. Set it up
once:

1. **BIOS, once, by hand** — enable "Wake on LAN" / "Power On By PCI-E" and
   disable ErP / Deep Sleep (S5) if the option exists (ErP cuts standby
   power, which WoL needs).
2. **Desktop, once** — `deploy/enable-wol.sh` (auto-runs under sudo; sets the
   NetworkManager profile to wake on magic packet and verifies with
   ethtool).
3. **Laptop, once** — install the sender on fedora-1 (always on, same LAN):
   `install -m 755 deploy/wake-desktop ~/.local/bin/wake-desktop`. No root
   needed — it only broadcasts a UDP magic packet.

Then, from the Mac:

```sh
ssh laptop wake-desktop   # send the magic packet
# wait ~30 s for the desktop to boot and join the tailnet
ssh desktop               # or open https://agent.intern.deepwa7er.net
```

Why the laptop: a magic packet is an L2 broadcast and must originate on the
desktop's LAN; the VPS is not on that LAN and cannot do it. fedora-1 is the
always-on host there.

## Access

From any device on the tailnet (the phone needs Tailscale):

```
https://agent.intern.deepwa7er.net
```

No password — the tailnet is the security boundary. If the domain is
unreachable, fall back to the desktop's tailnet IP directly:
`http://fedora.tailcfab97.ts.net:8120`.

The https URL is an installable PWA (manifest + service worker): "Add to
Home Screen" gives a standalone window. The worker's policy is asset-only
caching — digested files from the cache, transcripts and streams always from
the network — plus the offline page when the tailnet is unreachable. The
http fallback URL has no service worker (a secure context is required) and
simply works as a normal page.

The Mac instance still serves the same app at
`http://deepwater-1.tailcfab97.ts.net:8120` until it is retired.

## Caveats

- **The host must be awake.** The phone URL resolves to the tailnet node
  serving skiff (currently the desktop, fedora); sleeping or powered-off means
  no response. The same holds for the Mac instance while it runs. If the
  desktop is powered off, bring it back with Wake-on-LAN — see "Remote
  power-on (Wake-on-LAN)" above.
- **Tailnet-only.** Rails binds the tailnet IP, so skiff is reachable only
  inside the tailnet.
- **Every harness is effectively remote code execution on the host.** The
  tailnet is the security boundary — anyone on the tailnet can drive
  shell-capable agent runs through the phone UI, and muse runs are spawned
  `--yolo` (no sandbox, no approvals) by design. The bridge on
  127.0.0.1:4120 still guards its own HTTP with basic auth
  (`SKIFF_BRIDGE_PASSWORD` in the secrets file); keep that password out of
  reach.
- **Bridge-driven runs only are tracked live.** A run started from a CLI
  (e.g. `pi` or `muse` at a terminal) still streams its committed messages
  onto the phone through the file watchers, but the working indicator and
  the token-level overlay exist only for runs the bridge itself started.

## Testing

```sh
bin/rails test
bin/rails test test/lib/bridge_client_test.rb
bin/rails test test/controllers/sessions_test.rb
bin/rails test test/controllers/stream_test.rb
node --test bridge/test/*.test.js
```
