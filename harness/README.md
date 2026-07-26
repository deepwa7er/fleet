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
run yolo (commands execute without confirmation). Sessions are in-memory:
restarting the server ends them.

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

## The REPL

```sh
cargo build -p harness
./target/debug/harness                 # REPL
./target/debug/harness -p "fix the typo in README.md"   # one-shot
./target/debug/harness --no-yolo -p "run the tests and report" # confirm each command
./target/debug/harness --model kimi-for-coding          # K2.7 Code instead of K3
```

Multi-turn chat with history (rustyline). `/help` lists the commands: `exit` /
`/quit` / Ctrl-D quits, `/reset` clears history, `/context` shows the context
footprint, `/model` shows requested vs API-reported model, `/system` prints
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

## System prompt

The base system prompt lives in `~/.config/harness/system.md` — created with
the default content on first run. Placeholders `{cwd}`, `{os}`, `{date}` are
substituted at load; HTML comments are stripped. Appended after it, in order:
`AGENTS.md` from the session's working directory, `$KIMI_SYSTEM_PROMPT`,
`--system-file`, `--system`.

Env vars: `KIMI_API_KEY`, `KIMI_MODEL` (default `k3`), `KIMI_BASE_URL`,
`KIMI_CODE_HOME`, `KIMI_SYSTEM_PROMPT`, `KIMI_CONTEXT_WINDOW` (makes `/context`
show the last request as a percentage of the window).

## Deliberate limitations

- No context compaction — long sessions just grow; watch `/context` in the
  REPL and `/reset` (or Reset in the web UI) when it gets slow or expensive.
- No sandbox or permission system: yolo mode is the default everywhere and
  the only mode in the web UI. The model can run anything you can, in whatever
  directory the session points at. The tailnet is the only boundary.
- The credentials file is shared without locking; concurrent refreshes (CLI,
  REPL, web sessions) can make one writer's token stale, which self-heals via
  reload-from-disk on the next 401.
- Web sessions are ephemeral (in-memory); a server restart ends them all.
- `/model` can only report the served model if the API includes a `model`
  field in chat responses.
