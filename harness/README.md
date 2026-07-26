# harness

A minimal coding-agent harness for the Kimi Code subscription API — our own
tiny version of Kimi Code. One library (`src/`, the `harness` crate) around
one agent loop, with two frontends:

- **`harness`** — the terminal REPL (or one-shot `--prompt`). Responses stream
  over SSE: reasoning streams dimmed to stderr, answer text streams through a
  markdown renderer to stdout, and the spinner only covers time-to-first-token.
- **`harness serve`** — the web UI, bound to this Mac's Tailscale IPv4
  (discovered at startup from the 100.64.0.0/10 range, so it's tailnet-only;
  no auth — the tailnet is the boundary, same trust model as the rest of the
  fleet). Chat from any tailnet device while the MacBook is on.

Both frontends implement the same `TurnIo` trait (`src/agent.rs`): the loop
emits streamed deltas, notes, and tool activity, and pulls mid-turn input
(steering interjections, ask_user answers, run_command confirmation) through
it. `harness.py` is the original Python prototype, kept for reference.

## The web UI

`harness serve [--bind <ip>] [--port 8098]` serves an embedded single-page UI
(deliberately styled outside the fleet's U.S. Graphics design system — a soft,
minimal Jony Ive-style theme) plus a small JSON API:

```
GET    /api/healthz
GET    /api/sessions                      list sessions (id, cwd, busy)
POST   /api/sessions    {cwd?, model?}    create (cwd: absolute, ~/, or ~-relative)
DELETE /api/sessions/{id}                 end the session
POST   /api/sessions/{id}/messages {text} send a message (mid-turn = steer)
POST   /api/sessions/{id}/interrupt       abort the current turn
POST   /api/sessions/{id}/reset           clear history (409 while busy)
GET    /api/sessions/{id}/events          SSE: full replay, then live events
```

Each session is a driver task owning a library `Session` with its own working
directory: relative tool paths resolve against it, commands run in it, and its
system prompt and AGENTS.md come from it — concurrent web sessions (and the
REPL) never share a process-global cwd or interrupt bit. Web sessions always
run yolo (commands execute without confirmation).

### Sessions survive restarts

Web sessions are stored in SQLite (`--db`, else `$HARNESS_DB`, else
`~/.local/share/harness/harness.db`) and come back on startup as ordinary live
sessions you can keep talking to — a relaunch, a reboot, or a rebuild no
longer ends every conversation. Two logs persist, and they are deliberately
different:

- **the context** — the model's message history, restored verbatim so a
  resumed session remembers what was said. A reset deletes it, because that is
  what a reset means.
- **the transcript** — the event stream a browser renders. Append-only, and
  kept *across* a reset: the stored `Reset` event makes a replaying tab clear
  at the same point a live tab did, so the transcript stays a record of the
  whole session rather than only its current context.

Deleting a session (the `×` in the sidebar) removes both.

The system prompt is not stored. It is composed fresh on restore, so edits to
`~/.config/harness/system.md` or a project's `AGENTS.md` take effect on the
next start instead of being frozen into every old session.

Streaming deltas are coalesced before they are written: a turn emits thousands
of per-token events but stores the handful of blocks they build up to. Live
viewers still receive every delta — coalescing is the write path, not the
broadcast — so a tab that reconnects mid-turn still catches up token by token
from the in-memory replay buffer.

To run it while the MacBook is on, install the launchd agent:

```sh
harness/deploy/provision.sh   # release build → ~/.local/bin + LaunchAgent
```

It builds a native release binary, installs it to `~/.local/bin/harness`, and
bootstraps `net.deepwa7er.harness` (RunAtLoad + KeepAlive, logs to
`~/Library/Logs/harness.log`). The plist sets PATH to include
`/opt/homebrew/bin` — launchd's default PATH lacks it and the agent's tools
need `rg`. There is no `deploy.toml`: harness is not a VPS service, so tugboat
rightly doesn't discover it.

### Context compaction

Long sessions no longer just grow. When a *measured* prompt passes 75% of the
context window, the loop replaces the older messages with a model-written
summary and keeps the newest ones verbatim — so a session can run past the
window instead of walking into a context-length error with no recovery.

```
[compacting: last prompt was 4011 tokens, over 75% of the 4000 window]
[compacted 7 older messages into a summary; kept the newest 2]
```

The window comes from `KIMI_CONTEXT_WINDOW`, else K3's 1,000,000 — so in
practice this fires on genuinely long sessions (past ~750k tokens) rather than
routinely. The retained tail is sized to ~30% of the window, so a compaction
buys back real room rather than triggering again on the next turn.

**If you switch models, set `KIMI_CONTEXT_WINDOW`.** The default tracks K3 and
the API does not report a window; pointing `--model` at something smaller
without setting it is the one direction that gets expensive, because
compaction would then trigger too late to save the request.

Three things make it safe rather than merely clever:

- **It never cuts inside a tool run.** A retained tail may not begin with a
  tool result whose call is in the dropped prefix — the API requires the pair,
  and a history that violates it fails every later request.
- **It only runs at the top of the loop**, the one point where every tool call
  issued so far has its result appended.
- **Failure changes nothing.** If the summary request errors or comes back
  empty, the history is left untouched and a note says so. Running out of
  context is bad; silently destroying the conversation because a network call
  failed is worse.

The summary request is sent without tools (offering them invites a tool call
where prose was asked for) and its streamed output never reaches the
transcript, though Ctrl-C / Stop still aborts it. A line typed during
compaction stays queued and lands as an ordinary interjection afterwards.

`/compact` in the REPL forces one immediately, down to the same ~30% tail. At
K3's window that means it reports "nothing old enough to compact" until a
session is genuinely large — which is the honest answer, not a failure. To
watch the machinery work, run with a small `KIMI_CONTEXT_WINDOW`.

## The REPL

```sh
cargo build -p harness
./target/debug/harness                 # REPL
./target/debug/harness -p "fix the typo in README.md"   # one-shot
./target/debug/harness --no-yolo -p "run the tests and report" # confirm each command
./target/debug/harness --model kimi-for-coding          # K2.7 Code instead of K3
```

Multi-turn chat with history (rustyline). `/help` lists the commands: `exit` /
`/quit` / Ctrl-D quits, `/reset` clears history, `/compact` summarizes older
messages now, `/context` shows the context footprint (and where it sits
against the window), `/model` shows requested vs API-reported model, `/system` prints
the system prompt, `/usage` shows subscription quota, `/yolo` toggles command
auto-approval live. Ctrl-C at the prompt quits; Ctrl-C mid-turn aborts the
current turn; a second Ctrl-C force-quits. You can **steer the model
mid-turn**: type a line and hit Enter while it works — the in-flight request
is cancelled, your line is appended to the history, and the turn continues
with your guidance.

## Auth

`KIMI_API_KEY` wins; otherwise the harness reuses the Kimi Code CLI's OAuth
login from `~/.kimi-code/credentials/kimi-code.json`, refreshing it itself
(access tokens live ~15 minutes) and writing the rotated pair back to the
shared file atomically, so the CLI keeps using it too. If the refresh token
itself is dead, run `kimi` once to re-login. Under launchd there is no shell
environment, so in practice serve mode always uses the OAuth path — the CLI
login must be present.

Because the server *rotates* the refresh token on every use, the whole read →
refresh → write cycle runs under an advisory `flock` on a sibling
`kimi-code.json.lock`. A process that waits for the lock re-reads the file
first, so it adopts the winner's new pair instead of POSTing a refresh token
the winner already spent — which also collapses a burst of concurrent
expiries (several web sessions at once) into a single network refresh.

## System prompt

The base system prompt lives in `~/.config/harness/system.md` — created with
the default content on first run. Placeholders `{cwd}`, `{os}`, `{date}` are
substituted at load; HTML comments are stripped. Appended after it, in order:
`AGENTS.md` from the session's working directory, `$KIMI_SYSTEM_PROMPT`,
`--system-file`, `--system`.

Env vars: `KIMI_API_KEY`, `KIMI_MODEL` (default `k3`), `KIMI_BASE_URL`,
`KIMI_CODE_HOME`, `KIMI_SYSTEM_PROMPT`, `KIMI_CONTEXT_WINDOW` (the window
compaction triggers against and `/context` reports on; default 1,000,000 =
K3's), `HARNESS_DB` (the serve session database; `--db` wins over it).

## Tests

`cargo test -p harness`. Nothing here touches the network, your credentials, or
`~/.config/harness` — the loop tests (`src/loop_tests.rs`) run against an axum
server on an ephemeral loopback port that replays scripted SSE and records what
was sent to it, and they build a `Session` directly rather than through
`Session::start`, which would load real credentials and create `system.md`.

They cover the paths where a bug is expensive rather than annoying: a tool-call
round trip feeding its result back, `MAX_TURNS` stopping instead of spinning, an
API error ending the turn without killing the session, guidance arriving at a
tool boundary still answering *every* call (an unanswered one makes the history
permanently unusable), compaction firing inside the loop and asking without
tools, and a failed summary leaving the history untouched.

The 401-retry loop is **not** covered: it needs a credentials file and an OAuth
endpoint to rotate against. Its pieces are tested in `auth.rs`; the API-key 401
path, where no retry is possible, is covered here.

## Deliberate limitations

- The context window is not something the API reports. The default (1,000,000)
  tracks **K3 specifically**, so it is only right for the default model — see
  the compaction section if you change `--model` / `KIMI_MODEL`.
- The summarizer sees tool results clipped to 500 chars each. It is told so
  (and told the tool succeeded), but a summary can still be thinner than the
  transcript it replaces.
- No sandbox or permission system: yolo mode is the default everywhere and
  the only mode in the web UI. The model can run anything you can, in whatever
  directory the session points at. The tailnet is the only boundary.
- Credential refreshes are serialized between harness processes (REPL and web
  sessions) by an advisory lock on `kimi-code.json.lock`, but the Kimi Code CLI
  does not take that lock. A CLI refresh concurrent with ours can still leave
  one side holding a rotated-away token; that self-heals via reload-from-disk
  on the next 401.
- The REPL is not persisted — only `harness serve` sessions are. A terminal
  session still ends when you quit.
- Nothing prunes the session database: it grows until you delete sessions from
  the UI. And because harness has no `deploy.toml` (it is not a VPS service),
  `fleet-backup` does not know about that database — back it up yourself if
  the transcripts matter to you.
- A crash *mid-turn* loses the rest of that turn. Messages are recorded as the
  loop appends them, so the work up to the last completed message survives; any
  tool call the process died before answering is given a synthetic "harness
  restarted" result on restore, because the API requires every call to be
  answered and a history with a hole in it would fail every later request.
- `/model` can only report the served model if the API includes a `model`
  field in chat responses.
