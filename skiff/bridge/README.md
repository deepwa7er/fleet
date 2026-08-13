# skiff-pi-bridge

Zero-dependency Node ESM HTTP bridge: it speaks the opencode HTTP API that
skiff's `app/lib/opencode_client.rb` already knows, but is backed by pi
(`pi --mode rpc`, a JSONL protocol over stdin/stdout) instead of an opencode
server. The bridge reads pi's session files for history and keeps one live pi
process per active session for prompting, aborting, and streaming.

## How it fits together

- `lib/session-store.js` — pure, file-based reader/mapper for pi's v3 JSONL
  session format (leaf-branch walking, message mapping, toolResult folding).
- `lib/pi-rpc.js` — strict-JSONL RPC client (`PiProcess`) and the process pool
  (`PiPool`): id-correlated commands, event handling, in-flight assistant
  message assembly, extension-dialog auto-cancel, LRU eviction.
- `server.js` — the HTTP surface plus the two places process state is
  composed with file state: the streaming overlay and newborn sessions.

Two pi behaviors (verified live) shape the design:

1. **Responses arrive out of order.** Commands carry an `id` and responses
   echo it; correlation is by id, never by arrival order.
2. **`new_session` writes no file.** A created session exists only in its pi
   process until the first message is persisted. The bridge therefore serves
   newborn sessions from the pool until their file appears.

pi also emits fire-and-forget `extension_ui_request` events (setWidget,
setStatus) before any command; those are ignored, while dialog methods
(select/confirm/input/editor) are auto-cancelled so a prompt never deadlocks.

## Session storage: one layout, both sides

The default deployment runs with **no `PI_SESSION_DIR`** — the bridge scans
`~/.pi/agent/sessions` (pi's own default, honoring
`PI_CODING_AGENT_SESSION_DIR` when set) and spawns pi **without
`--session-dir`**, so pi writes sessions in its native per-cwd layout
(`--<cwd>--/` buckets). That is exactly the layout the pi CLI reads and
writes, so the phone sees and drives the same sessions as `pi -c` / `pi -r`,
and sessions created from the phone appear in the CLI's project listings.

An explicit `PI_SESSION_DIR` is the exception: the bridge then passes
`--session-dir` to its spawned processes so scanning and writing stay in one
place. That makes pi write sessions *flat* into the override dir (no per-cwd
buckets), so sessions created through the phone will not appear in the CLI's
project-scoped listings — acceptable, since a custom dir has no native pi
layout to preserve.

## Env contract

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENCODE_SERVER_PASSWORD` | — (required, fail-fast at boot) | basic-auth password skiff's client uses (username `opencode`) |
| `PI_BRIDGE_HOST` | `127.0.0.1` | bind host |
| `PI_BRIDGE_PORT` | `4120` | bind port |
| `PI_SESSION_DIR` | pi's default (`~/.pi/agent/sessions`, honoring `PI_CODING_AGENT_SESSION_DIR`) | where the bridge scans for session files; when set, spawned pi processes also get `--session-dir` (flat layout — see above) |
| `PI_BINARY` | `pi` | the pi executable; resolved at boot: PATH first, then `~/.local/bin/pi` and `~/bin/pi` (systemd/launchd PATHs omit `~/.local/bin`); set to an absolute path if pi lives elsewhere |
| `PI_BRIDGE_CWD` | `~/code` | spawn cwd for new sessions and fallback for unreadable headers |
| `PI_BRIDGE_MAX_PROCESSES` | `8` | pool cap; oldest idle process is evicted (LRU) |

Prompt text and the password are never logged.

## Endpoint mapping

| opencode endpoint | Backed by |
| --- | --- |
| `GET /global/health` | static `{"status":"ok"}` |
| `GET /session` | scan of `PI_SESSION_DIR` (+ newborn sessions from the pool) |
| `GET /session/{id}` | one session object (404 unknown) |
| `GET /session/{id}/message` | leaf-branch transcript + in-flight streaming overlay |
| `GET /session/{id}/stream` | SSE: snapshot + append/replace/remove/working/orchestrator events (see lib/stream-registry.js) |
| `POST /session` | bare `pi --mode rpc` → `new_session` → `set_session_name` → `get_state`; 201 `{"id":...}` |
| `POST /session/{id}/prompt_async` | `{"type":"prompt"}` to the session's live process; 200 accepted, 404 unknown, 502 pi failure |
| `POST /session/{id}/name` | `{"type":"set_session_name"}` to the session's live process; the name is persisted as a `session_info` entry; 200 `{"ok":true}`, 404 unknown, 400 empty name, 502 pi failure |
| `POST /session/{id}/abort` | `{"type":"abort"}`; 204 |
| `GET /session/status` | `{"<id>":{"type":"busy"}}` per running agent (agent_start..agent_settled); kept for the opencode API contract — skiff's view now gets working state from the stream's `working` events |

All bodies are JSON; errors are `{"error":"..."}`.

## Running

```sh
OPENCODE_SERVER_PASSWORD=... node bridge/server.js
# optional: PI_SESSION_DIR=... PI_BINARY=pi PI_BRIDGE_PORT=4120
```

## Testing

Tests use a scripted fake pi (`test/fixtures/fake-pi.mjs`) — no LLM involved.

```sh
node --test bridge/test/*.test.js
```

> Note: `node --test bridge/test/` (directory argument) does not work on
> Node ≥22: `--test` path arguments are glob patterns and directories are no
> longer expanded, so the directory itself is treated as a test file. Run the
> files directly as above.
