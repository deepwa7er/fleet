# skiff-bridge

Zero-dependency Node ESM HTTP bridge serving coding-agent sessions from
three harnesses — pi, Meta's Muse Code, and opencode — behind one JSON API
for skiff's `app/lib/bridge_client.rb`. Session ids on the wire are
harness-qualified (`pi:…`, `muse:…`, `opencode:…`); every session carries
its harness name and a `capabilities` object (rename and the orchestrator
toggle exist only where the harness supports them).

## How it fits together

Harness-agnostic core:

- `server.js` — routing, basic auth, body handling, and the dispatch from a
  wire session id to its harness. A harness whose binary is missing on a
  host degrades to a stub that reports itself in the session list's `errors`
  map — visible, never a dead bridge.
- `lib/stream-registry.js` — per-session live state fanned out over SSE:
  the transcript diff, the in-flight overlay's position bookkeeping, and
  coalesced flushing. Everything harness-specific enters through a
  per-session DRIVER each harness supplies (see the driver contract in the
  module comment).
- `lib/file-tail.js` — JSONL tail with rewrite detection; waits through
  missing files *and* missing directory chains (muse creates its dated
  session directory only when the first run boots).
- `lib/ids.js`, `lib/errors.js`, `lib/jsonl.js`, `lib/resolve-binary.js` —
  the small shared pieces.

pi harness (`lib/pi-harness.js`):

- `lib/session-store.js` — pure, file-based reader/mapper for pi's v3 JSONL
  session format (leaf-branch walking, message mapping, toolResult folding).
- `lib/pi-rpc.js` — strict-JSONL RPC client (`PiProcess`) and the process
  pool (`PiPool`): id-correlated commands, event handling, in-flight
  assistant message assembly, extension-dialog auto-cancel, LRU eviction.
- Two pi behaviors (verified live) shape the design: responses arrive out of
  order (correlation is by id, never arrival order), and `new_session`
  writes no file (newborn sessions are served from the pool until their file
  appears). Dialog `extension_ui_request`s are auto-cancelled so a prompt
  never deadlocks.

muse harness (`lib/muse-harness.js`):

- `lib/muse-store.js` — pure reader/mapper for muse's event-sourced session
  logs (`sessions/YYYY/MM/DD/<uuid>/session.jsonl`; subagent children are
  excluded by the fixed depth). Reasoning is persisted encrypted by muse and
  never renders.
- `lib/muse-run.js` — one `muse exec --json --yolo --session-id <uuid>`
  child per prompt (muse has no long-lived RPC mode). The prompt travels by
  `--prompt-file` in a 0700 temp dir, never argv. Verified live against
  Muse Code 0.2.1: exec RESUMES an existing session by id;
  `run.output.delta` stdout events are incremental chunks (they feed the
  overlay); committed messages appear only in the session file; an
  interrupted run (SIGINT) exits with no terminal record anywhere, so the
  child's exit owns convergence; muse recovers a hard-killed session's lock
  on the next run.
- Newborn sessions are just a minted uuid + cwd — no process is spawned at
  create time, and the record stays visible until the first run's file
  appears.

opencode harness (`lib/opencode-harness.js`):

- `lib/opencode-api.js` — minimal client for a headless `opencode serve`
  (its own systemd user unit, `deploy/opencode-serve.service`, loopback
  :4130). opencode owns its sessions end to end, so the adapter translates:
  session/message shapes map nearly 1:1 (skiff's `{ info, parts }` shape
  descends from opencode's), prompts/aborts/renames pass through, and the
  stream driver refetches the transcript (coalesced) on `/event` bus events
  and lets the registry diff — no overlay, because opencode updates the
  in-flight message inside its own message list.

## Session storage: one layout, both sides

The default deployment shares each harness's native session store with its
CLI, so the phone sees and drives the same sessions the terminal does:

- pi: **no `PI_SESSION_DIR`** — the bridge scans `~/.pi/agent/sessions`
  (honoring `PI_CODING_AGENT_SESSION_DIR`) and spawns pi **without**
  `--session-dir`, keeping pi's native per-cwd layout (`--<cwd>--/`
  buckets). An explicit `PI_SESSION_DIR` switches spawned processes onto
  `--session-dir` (flat layout) so scanning and writing stay in one place.
- muse: the bridge resolves `$XDG_DATA_HOME/muse/sessions` (default
  `~/.local/share/muse/sessions`) exactly as muse does. An explicit override
  (tests) must end in `/muse/sessions`; spawned muse children then get
  `XDG_DATA_HOME=<root>` — muse's own mechanism — so both sides stay in one
  place.
- opencode: `opencode serve` owns its store; the bridge holds nothing.

## Env contract

| Variable | Default | Purpose |
| --- | --- | --- |
| `SKIFF_BRIDGE_PASSWORD` | — (required, fail-fast at boot) | basic-auth password skiff's client uses (username `skiff`) |
| `SKIFF_BRIDGE_HOST` | `127.0.0.1` | bind host |
| `SKIFF_BRIDGE_PORT` | `4120` | bind port |
| `SKIFF_BRIDGE_CWD` | `~/code` | cwd for sessions created from the phone (all harnesses) |
| `PI_SESSION_DIR` | pi's default (see above) | pi scan dir; when set, spawned pi processes also get `--session-dir` |
| `PI_BINARY` | `pi` | resolved at boot: PATH, then `~/.local/bin/pi` and `~/bin/pi` (systemd/launchd PATHs omit `~/.local/bin`) |
| `PI_BRIDGE_MAX_PROCESSES` | `8` | pi pool cap; oldest idle process is evicted (LRU) |
| `MUSE_BINARY` | `muse` | resolved at boot like `PI_BINARY` |
| `OPENCODE_SERVE_URL` | `http://127.0.0.1:4130` | the headless `opencode serve` this bridge's opencode harness talks to |

Prompt text and the password are never logged.

## Endpoint mapping

| Endpoint | Behavior |
| --- | --- |
| `GET /global/health` | static `{"status":"ok"}` |
| `GET /session` | `{"sessions":[…],"errors":{…}}` — every harness's sessions merged (tagged with `harness` + `capabilities`), plus per-harness failures by name |
| `POST /session` | `{"harness":"pi"\|"muse"\|"opencode","title":…}` → 201 `{"id":"<harness>:<id>"}`; 400 without a valid harness |
| `GET /session/{id}` | one tagged session object (404 unknown) |
| `GET /session/{id}/message` | transcript + in-flight streaming overlay |
| `GET /session/{id}/stream` | SSE: snapshot + append/replace/remove/working/orchestrator events, plus a `heartbeat` every 15s for liveness (see lib/stream-registry.js) |
| `POST /session/{id}/prompt_async` | one text part → the harness's prompt surface; 200 accepted, 404 unknown, 409 a muse run already active, 502 harness failure |
| `POST /session/{id}/name` | rename, where `capabilities.rename` (pi, opencode); 400 for muse |
| `GET /harness/{name}/models` | the models the harness's sessions can switch to (`capabilities.model` — pi only: `pi --list-models`, parsed and briefly cached); 400 elsewhere |
| `POST /session/{id}/model` | `{"provider":…,"id":…}` → pi's `set_model` (appends a model_change entry; pi also keeps the choice as its default); 400 for unknown models and modelless harnesses |
| `POST /session/{id}/orchestrator` | pi's extension toggle; 400 for every other harness |
| `POST /session/{id}/abort` | 204; pi sends `abort`, muse SIGINTs the run's child, opencode POSTs its abort |
| `GET /session/status` | `{"<harness>:<id>":{"type":"busy"}}` for runs the bridge itself drives (pi, muse); opencode's working state surfaces through the stream |

All bodies are JSON; errors are `{"error":"..."}`.

## Running

```sh
SKIFF_BRIDGE_PASSWORD=... node bridge/server.js
# optional: PI_SESSION_DIR=... PI_BINARY=pi MUSE_BINARY=muse \
#           OPENCODE_SERVE_URL=http://127.0.0.1:4130 SKIFF_BRIDGE_PORT=4120
```

## Testing

Tests use scripted fakes — no LLM involved: `test/fixtures/fake-pi.mjs`
(stands in for `pi --mode rpc`), `test/fixtures/fake-muse.mjs` (stands in
for `muse exec --json`, including the XDG session layout and the
no-terminal-record SIGINT death), and `test/fixtures/fake-opencode.mjs` (an
in-process `opencode serve` with the `/event` bus).

```sh
node --test bridge/test/*.test.js
```

> Note: `node --test bridge/test/` (directory argument) does not work on
> Node ≥22: `--test` path arguments are glob patterns and directories are no
> longer expanded, so the directory itself is treated as a test file. Run the
> files directly as above.
